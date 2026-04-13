use crate::ia::selectors::query_selector;
use crate::ia::types::*;

/// Contact card — separate FSM for user profile cards.
struct ContactCardStateImpl;

impl IAState for ContactCardStateImpl {
    fn fsm(&self) -> &str {
        "contactCard"
    }
    fn id(&self) -> &str {
        "contact_card"
    }

    fn identify(&self, args: &IdentifyArgs) -> Result<IdentifyResult, String> {
        let wechat_id_label = query_selector(args.a11y, r#"label[name=/^(WeChat ID:|微信号:)$/]"#);
        Ok(IdentifyResult {
            identified: wechat_id_label.is_some(),
            frame: None,
        })
    }

    fn reduce(&self, args: &ReduceArgs) -> AppState {
        let wechat_id_label = query_selector(args.a11y, r#"label[name=/^(WeChat ID:|微信号:)$/]"#);

        // The actual ID is in a sibling label right after "WeChat ID:"
        let wechat_id = wechat_id_label.and_then(|_label| {
            // Look for a sibling label in the parent's children
            // This requires tree traversal, which is simplified here
            None::<String> // TODO: implement sibling traversal
        });

        let contact_name = query_selector(args.a11y, "filler filler filler filler filler label")
            .map(|n| n.name.clone());

        let mut state = args.prev.clone();
        state.contact_card = Some(ContactCardState {
            wechat_id,
            contact_name,
        });
        state
    }
}

pub static CONTACT_CARD_STATE: std::sync::LazyLock<Box<dyn IAState>> =
    std::sync::LazyLock::new(|| Box::new(ContactCardStateImpl));
