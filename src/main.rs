use dotenv;
use env;
use reqwest;
use scraper::node::Element;
use scraper::{ElementRef, Html, Selector};
use std::collections::HashMap;
use std::error::Error;
use std::fs::{self, File};
use std::io::Write;
use tokio;

struct Config {
    dry: bool,
    proxy: bool,
    url: String,
    login: String,
    password: String,
}

struct Client {
    debt: f32,
    debit: f32,
    credit: f32,
}

impl Client {
    fn balance_comp(&self) -> f32 {
        self.debit + self.credit
    }
}

impl Config {
    fn build() -> Result<Self, &'static str> {
        dotenv::dotenv().expect("Could not read the .env file");
        Ok(Self {
            dry: env::var("DRY").is_ok(),
            proxy: env::var("PROXY").is_ok(),
            url: env::var("URL").map_err(|_| "URL variable is not set.")?,
            login: env::var("LOGIN").map_err(|_| "LOGIN variable is not set.")?,
            password: env::var("PASSWORD").map_err(|_| "PASSWORD variable is not set.")?,
        })
    }
}

const HIDDEN_FIELDS: [&'static str; 3] = ["VIEWSTATE", "VIEWSTATEGENERATOR", "EVENTVALIDATION"];

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let config = Config::build()?;
    let args = env::args().collect::<Vec<String>>();
    if args.len() < 2 {
        eprint!("No client name provided");
        std::process::exit(1);
    }
    let name = args[1].clone();
    let page;
    if !config.dry {
        page = fetch_page(&config).await?;
        let mut f = File::create("debug.html")?;
        f.write_all(page.as_bytes())?;
    } else {
        page = fs::read_to_string("debug.html")?;
    }
    let html = Html::parse_document(&page);
    let debt = parse_ending_balance_page(&html, &name);
    println!("{debt:?}");

    Ok(())
}

fn parse_ending_balance_page(page: &Html, client: &String) -> Option<f32> {
    let selector = Selector::parse("td[id$=tdClient]").unwrap();
    for el in page.select(&selector) {
        let name = el
            .select(&Selector::parse("a").unwrap())
            .next()
            .unwrap()
            .inner_html();

        if name.as_str() == client {
            let debt_selector = format!("#{}", el.value().id().unwrap().replace("Client", "Debt"));
            let debt_selector = Selector::parse(debt_selector.as_str()).unwrap();
            let Some(td) = page.select(&debt_selector).next() else {
                continue;
            };
            return td.inner_html().split(" ").next()?.parse::<f32>().ok();
        }
    }

    None
}

async fn fetch_page(config: &Config) -> Result<String, Box<dyn Error>> {
    let agent = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));
    let mut client = reqwest::Client::builder()
        .user_agent(agent)
        .cookie_store(true)
        .referer(true);
    if config.proxy {
        client = client.proxy(reqwest::Proxy::http("http://localhost:8080")?);
    }
    let client = client.build()?;

    let res = client.get(&config.url).send().await?.text().await?;

    let body = Html::parse_document(&res);
    let mut params = parse_hidden_fields(&body)?;
    params.extend([
        (String::from("txtLogin"), config.login.clone()),
        (String::from("txtPassword"), config.password.clone()),
        (String::from("btnLogin"), String::from("Login")),
    ]);

    Ok(client
        .post(&config.url)
        .form(&params)
        .send()
        .await?
        .text()
        .await?)
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
