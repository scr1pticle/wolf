use std::error::Error;
use tokio;
use wolf::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let app = App::new()?;
    run(&app).await?;
    let customer_data = app.get_balance()?;
    println!("{}", "-".repeat(50));
    println!("{: <30} | {: <10}", "name", "balance");
    println!("{}", "-".repeat(50));
    for customer in customer_data {
        println!(
            "{: <30} | {: <10} EUR",
            customer.name,
            customer.balance as f32 / 100f32
        );
    }

    Ok(())
}

// fn help_menu(comms: &[Command]) -> String {
//     comms
//         .iter()
//         .map(|command| {
//             let mut args_str = String::new();
//             command.arguments.as_ref().inspect(|args| {
//                 args_str = args
//                     .iter()
//                     .map(|arg| format!("[{arg}]"))
//                     .collect::<Vec<_>>()
//                     .join(" ");
//             });
//             format!(
//                 "{: <20} {} - {}",
//                 command.name, args_str, command.description
//             )
//         })
//         .collect::<Vec<_>>()
//         .join("\n")
// }
