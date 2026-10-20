//! Format compatibility for the pinned Linux 4.1.13.26 layout.
//! This application-wide MMKV format constant is not an account credential.
//! Server credentials remain in the account vault and owner-only profile files.
pub(crate) fn global_mmkv_format_key() -> zeroize::Zeroizing<Vec<u8>> {
    zeroize::Zeroizing::new(vec![
        120, 119, 101, 99, 104, 97, 116, 95, 99, 114, 121, 112, 116, 95, 107, 101,
    ])
}
