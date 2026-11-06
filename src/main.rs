use axum::{Router, routing::get};
use std::error::Error;
use tokio;
use wolf::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let app = App::new()?;
    // run(&app).await?;
    // let customer_data = app.get_balance()?;
    // println!("{}", "-".repeat(50));
    // println!("{: <30} | {: <10}", "name", "balance");
    // println!("{}", "-".repeat(50));
    // for customer in customer_data {
    //     println!(
    //         "{: <30} | {: <10} EUR",
    //         customer.name,
    //         customer.balance as f32 / 100f32
    //     );
    // }
    //
    let test = Router::new().route("/", get(|| async { "Hello, world!" }));
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, test).await.unwrap();
    Ok(())
}
