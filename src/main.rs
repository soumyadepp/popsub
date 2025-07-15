mod config;

use config::load_config;

fn main() {
    dotenvy::dotenv().ok(); // load .env

    let settings = load_config().expect("Failed to load config");
    println!("Starting server on {}:{}", settings.server.host, settings.server.port);
}
