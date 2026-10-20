//! mp-admin — minimal operator tooling for dev/bring-up: mint enrollment
//! tokens, create operators, list devices. Operates on the same SQLite DB as
//! server-main (MP_DATABASE_URL, default sqlite://mp.db).
//!
//!   mp-admin mint-token <RAW_TOKEN>
//!   mp-admin add-operator <USERNAME> <PASSWORD>
//!   mp-admin set-password <USERNAME>     (new password on stdin)
//!   mp-admin list-devices

use mp_contracts::OperatorRole;
use mp_server_main::{auth, db, registry, util};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let url = std::env::var("MP_DATABASE_URL").unwrap_or_else(|_| "sqlite://mp.db".to_string());
    let pool = db::connect(&url).await?;

    match args.get(1).map(String::as_str) {
        Some("mint-token") => {
            let raw = args.get(2).expect("usage: mint-token <RAW_TOKEN>");
            let now = util::now_utc_ns();
            // valid for 24h
            let expires = now + 24 * 3600 * 1_000_000_000;
            registry::mint_enrollment_token(&pool, &util::hash_token(raw), now, expires).await?;
            println!("minted enrollment token (expires in 24h)");
        }
        Some("add-operator") => {
            let user = args.get(2).expect("usage: add-operator <USER> <PASS>");
            let pass = args.get(3).expect("usage: add-operator <USER> <PASS>");
            auth::create_operator(&pool, user, pass, OperatorRole::Operator).await?;
            println!("operator '{user}' added");
        }
        Some("set-password") => {
            // from stdin, so it never shows in the process list or history
            let user = args.get(2).expect("usage: set-password <USER> < password");
            let mut pass = String::new();
            std::io::stdin().read_line(&mut pass)?;
            let pass = pass.trim_end_matches(['\r', '\n']);
            if pass.len() < 12 {
                eprintln!("refusing a password shorter than 12 characters");
                std::process::exit(2);
            }
            if !auth::set_operator_password(&pool, user, pass).await? {
                eprintln!("no operator '{user}'");
                std::process::exit(1);
            }
            println!("password of operator '{user}' changed");
        }
        Some("list-devices") => {
            for d in registry::list_devices(&pool).await? {
                println!(
                    "{}  platform={}  model={}  cfg_v={}",
                    d.device_id,
                    d.platform,
                    d.model.unwrap_or_default(),
                    d.logging_config_version
                );
            }
        }
        _ => {
            eprintln!("usage: mp-admin <mint-token RAW | add-operator USER PASS | set-password USER (stdin) | list-devices>");
            std::process::exit(2);
        }
    }
    Ok(())
}
