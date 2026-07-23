use dotenv;
use env;
use reqwest;
use scraper::{Html, Selector};
use std::collections::HashMap;
use std::error::Error;
use tokio;

const HIDDEN_FIELDS: [&'static str; 3] = ["VIEWSTATE", "VIEWSTATEGENERATOR", "EVENTVALIDATION"];

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    dotenv::dotenv()?;
    let url = env::var("URL")?;
    let agent = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));
    let mut client = reqwest::Client::builder()
        .user_agent(agent)
        .cookie_store(true)
        .referer(true);
    if env::var("PROXY").is_ok() {
        client = client.proxy(reqwest::Proxy::http("http://localhost:8080")?);
    }
    let client = client.build()?;

    let res = client.get(&url).send().await?.text().await?;

    let login = env::var("LOGIN")?;
    let pass = env::var("PASSWORD")?;

    let body = Html::parse_document(&res);
    let mut params = parse_hidden_fields(&body)?;
    params.extend([
        (String::from("txtLogin"), login),
        (String::from("txtPassword"), pass),
        (String::from("btnLogin"), String::from("Login")),
    ]);

    let res = client.post(&url).form(&params).send().await?.text().await?;
    println!("{res:?}");

    Ok(())
}

fn parse_hidden_fields(body: &Html) -> Result<HashMap<String, String>, &'static str> {
    let mut result = HashMap::new();
    for name in HIDDEN_FIELDS {
        let selector = String::from("#__") + name;
        let selector = Selector::parse(selector.as_str()).unwrap(); //cant fail
        let value = body
            .select(&selector)
            .next()
            .ok_or("Could not find the hidden element {name}")?
            .value()
            .attr("value")
            .ok_or("The hidden element {name} does not have a value")?;

        result.insert(String::from("__") + name, String::from(value));
    }
    Ok(result)
}
