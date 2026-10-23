use super::{decode_cdn_config, public_ipv4, CdnConfig, CdnLocation};
use crate::{protobuf as p, AccountState};
use anyhow::{bail, ensure, Result};
use std::{collections::BTreeSet, net::Ipv4Addr};

const MAX_BOOTSTRAP_BYTES: usize = 512 * 1024;
const MAX_RULE_BYTES: usize = 256 * 1024;
const MAX_CONFIGS: usize = 64;
const MAX_RULES: usize = 512;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LargeCdnRoute {
    Upload,
    Download,
    VideoDownload,
}

/// Decode the safe C2C routing table in an authenticated GetCDNDns response.
/// The safe CDN UIN belongs to that routing table and may differ from the account UIN.
pub fn decode_large_cdn_config(
    input: &[u8],
    account: &AccountState,
    route: LargeCdnRoute,
) -> Result<CdnConfig> {
    ensure!(
        input.len() <= MAX_BOOTSTRAP_BYTES,
        "CDN bootstrap too large"
    );
    // The normal group binds this authenticated response to the active account.
    // Its credentials are separate from the safe group's credentials.
    let normal = decode_cdn_config(input, account)?;
    let response = p::decode(input)?;
    let safe = p::decode(p::data(&response, 7)?)?;
    let version = number(&safe, 1)?;
    let uin = number(&safe, 2)?;
    ensure!(
        uin != 0 && version == normal.version,
        "Invalid safe CDN identity"
    );
    let safe_auth = p::unbuffer(p::data(&safe, 8)?)?;
    ensure!(
        !safe_auth.is_empty() && safe_auth.len() <= 65536,
        "Invalid safe CDN auth key"
    );

    let raw = p::unbuffer(p::data(&response, 6)?)?;
    ensure!(raw.len() <= MAX_RULE_BYTES, "CDN routing table too large");
    let table = p::decode(&raw)?;
    ensure!(
        number(&table, 1)? == version && number(&table, 2)? == uin,
        "Safe CDN routing identity mismatch"
    );
    let location = CdnLocation {
        province: optional_number(&table, 3)?,
        city: optional_number(&table, 4)?,
        isp: optional_number(&table, 5)?,
    };

    // The official client sorts rules by signed sequence before taking the
    // first match. A zero in a predicate is a wildcard, not a priority score.
    let mut selected: Option<(i32, u32, bool)> = None;
    for raw_rule in repeated(&table, 8, MAX_RULES)? {
        let rule = p::decode(raw_rule)?;
        ensure!(
            rule.iter().all(|f| (1..=6).contains(&f.number)),
            "Unsupported CDN routing predicate"
        );
        let sequence = number(&rule, 1)? as i32;
        // Uploadbig promotes routing type to 7, independently of the actual
        // request type. Video download retains actual type 4 and operation 2.
        let predicates = [
            (number(&rule, 2)?, 1),
            (number(&rule, 3)?, 1),
            (
                number(&rule, 4)?,
                if route == LargeCdnRoute::VideoDownload {
                    4
                } else {
                    7
                },
            ),
            (
                number(&rule, 5)?,
                match route {
                    LargeCdnRoute::Upload => 1,
                    LargeCdnRoute::Download | LargeCdnRoute::VideoDownload => 2,
                },
            ),
        ];
        let proid = number(&rule, 6)?;
        ensure!(proid != 0, "Invalid CDN routing target");
        if !predicates
            .iter()
            .all(|(predicate, actual)| *predicate == 0 || predicate == actual)
        {
            continue;
        }
        match selected {
            Some((priority, previous, _)) if priority == sequence => {
                selected = Some((priority, previous, true));
            }
            Some((priority, _, _)) if priority < sequence => {}
            _ => selected = Some((sequence, proid, false)),
        }
    }
    let (_, proid, ambiguous) =
        selected.ok_or_else(|| anyhow::anyhow!("No large-file CDN route"))?;
    ensure!(!ambiguous, "Ambiguous CDN routing priority");

    let mut config_ids = BTreeSet::new();
    let mut selected_config = None;
    for raw_config in repeated(&table, 7, MAX_CONFIGS)? {
        let config = p::decode(raw_config)?;
        let id = number(&config, 1)?;
        ensure!(id != 0 && config_ids.insert(id), "Ambiguous CDN config ID");
        if id != proid {
            continue;
        }
        let auth_key = p::data(&config, 8)?.to_vec();
        let front_id = number(&config, 2)?;
        ensure!(front_id != 0, "Missing CDN front ID");
        let mut server_ips = Vec::new();
        for field in config.iter().filter(|f| f.number == 3) {
            ensure!(server_ips.len() < 32, "Too many CDN endpoints");
            let p::Value::Varint(value) = field.value else {
                bail!("Invalid CDN endpoint")
            };
            let ip = Ipv4Addr::from(wire_u32(value)?.to_le_bytes());
            ensure!(public_ipv4(ip), "CDN endpoint is not public IPv4");
            ensure!(!server_ips.contains(&ip), "Duplicate CDN endpoint");
            server_ips.push(ip);
        }
        // This transport implements the observed cleartext HTTP carrier on 443.
        let mut has_port_443 = false;
        for (index, port) in config.iter().filter(|f| f.number == 7).enumerate() {
            ensure!(index < 16, "Too many CDN ports");
            let p::Value::Varint(value) = port.value else {
                bail!("Invalid CDN port")
            };
            ensure!((1..=65535).contains(&value), "Invalid CDN port");
            has_port_443 |= value == 443;
        }
        ensure!(has_port_443, "CDN route does not advertise port 443");
        let result = CdnConfig {
            version,
            uin: u64::from(uin),
            client_version: account.device.client_version as u32,
            client_os_type: account.device.device_type.clone(),
            auth_key,
            server_ips,
            front_id,
            location: Some(location),
        };
        result.validate()?;
        selected_config = Some(result);
    }
    selected_config.ok_or_else(|| anyhow::anyhow!("CDN routing target is missing"))
}

fn number(fields: &[p::Field], field: u32) -> Result<u32> {
    wire_u32(p::scalar(fields, field)?)
}

fn optional_number(fields: &[p::Field], field: u32) -> Result<u32> {
    match p::optional(fields, field)? {
        Some(p::Value::Varint(value)) => wire_u32(*value),
        None => Ok(0),
        _ => bail!("Invalid CDN location number"),
    }
}

fn wire_u32(value: u64) -> Result<u32> {
    let low = value as u32;
    ensure!(
        value == u64::from(low) || value == (low as i32 as i64 as u64),
        "Invalid 32-bit CDN number"
    );
    Ok(low)
}

fn repeated(fields: &[p::Field], number: u32, max: usize) -> Result<Vec<&[u8]>> {
    let mut values = Vec::new();
    for field in fields.iter().filter(|f| f.number == number) {
        ensure!(values.len() < max, "Too many CDN routing records");
        let p::Value::Bytes(value) = &field.value else {
            bail!("Invalid CDN routing record")
        };
        values.push(value.as_slice());
    }
    ensure!(!values.is_empty(), "Missing CDN routing records");
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AuthSnapshot, DeviceProfile};

    const ACCOUNT_UIN: u32 = 123;
    const SAFE_UIN: u32 = 0xf1234567;

    fn state() -> AccountState {
        AccountState {
            schema: 1,
            device: DeviceProfile {
                device_id: vec![1; 16],
                device_type: "UnifiedPCLinux synthetic x86_64".into(),
                client_version: -227271398,
            },
            uin: ACCOUNT_UIN,
            own_username: "synthetic-user".into(),
            cookie: vec![5],
            session_key: vec![6; 16],
            request_key: vec![7; 24],
            response_key: vec![8; 24],
            auto_auth_key: vec![],
            key_info: vec![],
            database_password: vec![],
            auth_snapshot: AuthSnapshot::default(),
            long_host: "long.weixin.qq.com".into(),
            short_host: "short.weixin.qq.com".into(),
        }
    }

    fn rule(sequence: u32, biz: u32, app: u32, file: u32, op: u32, proid: u32) -> Vec<u8> {
        [sequence, biz, app, file, op, proid]
            .into_iter()
            .enumerate()
            .flat_map(|(i, value)| p::uint(i as u32 + 1, u64::from(value)))
            .collect()
    }

    fn config(id: u32, ip: [u8; 4], auth: &[u8]) -> Vec<u8> {
        [
            p::uint(1, u64::from(id)),
            p::uint(2, 1000 + u64::from(id)),
            p::int32(3, u32::from_le_bytes(ip) as i32),
            p::uint(7, 443),
            p::bytes(8, auth),
        ]
        .concat()
    }

    fn bootstrap(rules: &[Vec<u8>], configs: &[Vec<u8>]) -> Vec<u8> {
        let group = |uin| {
            [
                p::uint(1, 1),
                p::uint(2, u64::from(uin)),
                p::bytes(6, &p::bytes(1, b"8.8.8.8")),
                p::bytes(8, &p::buffer(&[7; 73])),
            ]
            .concat()
        };
        let table = [
            p::uint(1, 1),
            p::int32(2, SAFE_UIN as i32),
            p::uint(3, 1234),
            p::uint(4, 56),
            p::uint(5, 78),
            configs.iter().flat_map(|v| p::bytes(7, v)).collect(),
            rules.iter().flat_map(|v| p::bytes(8, v)).collect(),
        ]
        .concat();
        [
            p::bytes(1, &p::uint(1, 0)),
            p::bytes(2, &group(ACCOUNT_UIN)),
            p::bytes(7, &group(SAFE_UIN)),
            p::bytes(6, &p::buffer(&table)),
        ]
        .concat()
    }

    #[test]
    fn safe_identity_signed_ips_and_sequence_join_are_preserved() {
        let input = bootstrap(
            &[
                rule(12, 1, 0, 0, 0, 4),
                rule(2, 11, 0, 7, 0, 4),
                rule(3, 1, 0, 7, 1, 1),
            ],
            &[
                config(4, [9, 9, 9, 9], &[4; 73]),
                config(1, [8, 8, 8, 200], &[1; 73]),
            ],
        );
        let selected = decode_large_cdn_config(&input, &state(), LargeCdnRoute::Upload).unwrap();
        assert_eq!(selected.uin, u64::from(SAFE_UIN));
        assert_ne!(selected.uin, u64::from(ACCOUNT_UIN));
        assert_eq!(selected.front_id, 1001);
        assert_eq!(selected.auth_key, vec![1; 73]);
        assert_eq!(selected.server_ips, vec![Ipv4Addr::new(8, 8, 8, 200)]);
        let location = selected.location.unwrap();
        assert_eq!(location.province, 1234);
        assert_eq!(location.city, 56);
        assert_eq!(location.isp, 78);
        assert!(decode_cdn_config(&input, &state())
            .unwrap()
            .location
            .is_none());
        let fallback = decode_large_cdn_config(&input, &state(), LargeCdnRoute::Download).unwrap();
        assert_eq!(fallback.front_id, 1004);
    }

    #[test]
    fn route_must_match_all_predicates_and_join_one_config() {
        let configs = [config(1, [8, 8, 8, 8], &[1; 73])];
        for candidate in [
            rule(1, 2, 0, 7, 0, 1),
            rule(1, 1, 2, 7, 0, 1),
            rule(1, 1, 0, 5, 0, 1),
            rule(1, 1, 0, 7, 2, 1),
            rule(1, 1, 0, 7, 0, 9),
        ] {
            assert!(decode_large_cdn_config(
                &bootstrap(&[candidate], &configs),
                &state(),
                LargeCdnRoute::Upload
            )
            .is_err());
        }
        let rules = [rule(1, 1, 0, 7, 0, 1), rule(1, 1, 0, 0, 0, 1)];
        assert!(decode_large_cdn_config(
            &bootstrap(&rules, &configs),
            &state(),
            LargeCdnRoute::Upload
        )
        .is_err());
        let duplicated = [configs[0].clone(), configs[0].clone()];
        assert!(decode_large_cdn_config(
            &bootstrap(&[rules[0].clone()], &duplicated),
            &state(),
            LargeCdnRoute::Upload
        )
        .is_err());
    }

    #[test]
    fn upload_and_download_select_their_own_operation_with_routing_type_seven() {
        let input = bootstrap(
            &[
                rule(0, 1, 1, 5, 0, 3),
                rule(1, 1, 1, 7, 1, 1),
                rule(2, 1, 1, 7, 2, 2),
            ],
            &[
                config(3, [1, 1, 1, 1], &[3; 73]),
                config(2, [9, 9, 9, 9], &[2; 73]),
                config(1, [8, 8, 8, 8], &[1; 73]),
            ],
        );
        let upload = decode_large_cdn_config(&input, &state(), LargeCdnRoute::Upload).unwrap();
        let download = decode_large_cdn_config(&input, &state(), LargeCdnRoute::Download).unwrap();
        assert_eq!(upload.front_id, 1001);
        assert_eq!(download.front_id, 1002);
        assert_eq!(upload.auth_key, vec![1; 73]);
        assert_eq!(download.auth_key, vec![2; 73]);
    }

    #[test]
    fn video_download_selects_type_four_without_file_seven_fallback() {
        let configs = [
            config(1, [8, 8, 8, 8], &[1; 73]),
            config(2, [9, 9, 9, 9], &[2; 73]),
        ];
        let input = bootstrap(&[rule(1, 1, 1, 7, 2, 1), rule(2, 1, 1, 4, 2, 2)], &configs);
        let video =
            decode_large_cdn_config(&input, &state(), LargeCdnRoute::VideoDownload).unwrap();
        let file = decode_large_cdn_config(&input, &state(), LargeCdnRoute::Download).unwrap();
        assert_eq!(video.front_id, 1002);
        assert_eq!(video.auth_key, vec![2; 73]);
        assert_eq!(file.front_id, 1001);
        for wrong in [rule(1, 1, 1, 7, 2, 1), rule(1, 1, 1, 4, 1, 1)] {
            assert!(decode_large_cdn_config(
                &bootstrap(&[wrong], &configs),
                &state(),
                LargeCdnRoute::VideoDownload
            )
            .is_err());
        }
    }

    #[test]
    fn sequence_precedes_specificity_and_only_winning_priority_must_be_unique() {
        let configs = [
            config(1, [8, 8, 8, 8], &[1; 73]),
            config(4, [9, 9, 9, 9], &[4; 73]),
        ];
        let rules = [rule(3, 1, 0, 7, 0, 1), rule(1, 1, 0, 0, 0, 4)];
        assert_eq!(
            decode_large_cdn_config(
                &bootstrap(&rules, &configs),
                &state(),
                LargeCdnRoute::Upload
            )
            .unwrap()
            .front_id,
            1004
        );
        let rules = [
            rule(12, 1, 0, 0, 0, 4),
            rule(12, 1, 0, 7, 0, 4),
            rule(3, 1, 0, 7, 0, 1),
        ];
        assert_eq!(
            decode_large_cdn_config(
                &bootstrap(&rules, &configs),
                &state(),
                LargeCdnRoute::Upload
            )
            .unwrap()
            .front_id,
            1001
        );
    }

    #[test]
    fn routing_counts_authentication_and_future_predicates_are_limited() {
        let configs = [config(1, [8, 8, 8, 8], &[1; 73])];
        let valid = rule(3, 1, 0, 7, 0, 1);
        assert!(decode_large_cdn_config(
            &bootstrap(&vec![valid.clone(); MAX_RULES + 1], &configs),
            &state(),
            LargeCdnRoute::Upload
        )
        .is_err());
        assert!(decode_large_cdn_config(
            &bootstrap(
                std::slice::from_ref(&valid),
                &vec![configs[0].clone(); MAX_CONFIGS + 1]
            ),
            &state(),
            LargeCdnRoute::Upload
        )
        .is_err());
        let future = [valid.clone(), p::uint(7, 1)].concat();
        assert!(decode_large_cdn_config(
            &bootstrap(&[future], &configs),
            &state(),
            LargeCdnRoute::Upload
        )
        .is_err());
        for auth in [vec![], vec![1; 65537]] {
            assert!(decode_large_cdn_config(
                &bootstrap(
                    std::slice::from_ref(&valid),
                    &[config(1, [8, 8, 8, 8], &auth)]
                ),
                &state(),
                LargeCdnRoute::Upload
            )
            .is_err());
        }
        let excessive = vec![0; MAX_BOOTSTRAP_BYTES + 1];
        assert!(decode_large_cdn_config(&excessive, &state(), LargeCdnRoute::Upload).is_err());
    }

    #[test]
    fn routing_identity_and_raw_authentication_are_checked_independently() {
        let rules = [rule(3, 1, 0, 7, 0, 1)];
        let config = config(1, [8, 8, 8, 8], &[1; 73]);
        let response = bootstrap(&rules, &[config]);
        let fields = p::decode(&response).unwrap();
        let raw = p::unbuffer(p::data(&fields, 6).unwrap()).unwrap();
        let table = p::decode(&raw).unwrap();
        let altered_table = [
            p::uint(1, 1),
            p::uint(2, u64::from(SAFE_UIN - 1)),
            p::bytes(7, p::data(&table, 7).unwrap()),
            p::bytes(8, p::data(&table, 8).unwrap()),
        ]
        .concat();
        let altered = [
            p::bytes(1, p::data(&fields, 1).unwrap()),
            p::bytes(2, p::data(&fields, 2).unwrap()),
            p::bytes(7, p::data(&fields, 7).unwrap()),
            p::bytes(6, &p::buffer(&altered_table)),
        ]
        .concat();
        let error = decode_large_cdn_config(&altered, &state(), LargeCdnRoute::Upload)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("routing identity mismatch"));
        // Route auth is raw bytes and may differ from the legacy safe group's
        // auth. Unwrapping it as another SKBuiltinBuffer would corrupt it.
        let selected = decode_large_cdn_config(&response, &state(), LargeCdnRoute::Upload).unwrap();
        assert_eq!(selected.auth_key, vec![1; 73]);
    }

    #[test]
    fn malformed_identity_wrappers_and_untrusted_endpoints_are_rejected() {
        let rules = [rule(3, 1, 0, 7, 0, 1)];
        let input = bootstrap(&rules, &[config(1, [8, 8, 8, 8], &[1; 73])]);
        let mut other = state();
        other.uin += 1;
        assert!(decode_large_cdn_config(&input, &other, LargeCdnRoute::Upload).is_err());
        for ip in [[127, 0, 0, 1], [10, 0, 0, 1], [192, 0, 2, 1]] {
            assert!(decode_large_cdn_config(
                &bootstrap(&rules, &[config(1, ip, &[1; 73])]),
                &state(),
                LargeCdnRoute::Upload
            )
            .is_err());
        }
        let fields = p::decode(&input).unwrap();
        for replacement in [
            p::bytes(1, &p::int32(1, -13)),
            p::bytes(6, &[p::uint(1, 1), p::bytes(2, b"")].concat()),
            p::bytes(7, &[p::uint(1, 1), p::uint(2, 99)].concat()),
        ] {
            let replacement_field = p::decode(&replacement).unwrap()[0].number;
            let altered = fields
                .iter()
                .filter(|f| f.number != replacement_field)
                .map(|f| {
                    p::bytes(
                        f.number,
                        match &f.value {
                            p::Value::Bytes(b) => b,
                            _ => unreachable!(),
                        },
                    )
                })
                .chain(std::iter::once(replacement))
                .flatten()
                .collect::<Vec<_>>();
            assert!(decode_large_cdn_config(&altered, &state(), LargeCdnRoute::Upload).is_err());
        }
        assert!(wire_u32(1u64 << 32).is_err());
    }

    #[test]
    fn location_defaults_match_protobuf_and_malformed_values_are_rejected() {
        let rules = [rule(3, 1, 0, 7, 0, 1)];
        let input = bootstrap(&rules, &[config(1, [8, 8, 8, 8], &[1; 73])]);
        let response = p::decode(&input).unwrap();
        let raw = p::unbuffer(p::data(&response, 6).unwrap()).unwrap();
        let table = p::decode(&raw).unwrap();
        let without_location = [
            p::uint(1, 1),
            p::int32(2, SAFE_UIN as i32),
            p::bytes(7, p::data(&table, 7).unwrap()),
            p::bytes(8, p::data(&table, 8).unwrap()),
        ]
        .concat();
        let replace_table = |location: &[u8]| {
            [
                p::bytes(1, p::data(&response, 1).unwrap()),
                p::bytes(2, p::data(&response, 2).unwrap()),
                p::bytes(7, p::data(&response, 7).unwrap()),
                p::bytes(
                    6,
                    &p::buffer(&[without_location.clone(), location.to_vec()].concat()),
                ),
            ]
            .concat()
        };
        let selected =
            decode_large_cdn_config(&replace_table(&[]), &state(), LargeCdnRoute::Upload).unwrap();
        let location = selected.location.unwrap();
        assert_eq!((location.province, location.city, location.isp), (0, 0, 0));
        for malformed in [
            p::uint(3, 1u64 << 32),
            p::bytes(4, b"invalid"),
            [p::uint(5, 1), p::uint(5, 2)].concat(),
        ] {
            assert!(decode_large_cdn_config(
                &replace_table(&malformed),
                &state(),
                LargeCdnRoute::Upload
            )
            .is_err());
        }
    }
}
