use tokio::sync::Mutex;

pub static UI_OPERATION_LOCK: Mutex<()> = Mutex::const_new(());
