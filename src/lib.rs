use async_stream::try_stream;
use chrono::Utc;
use diacritics::remove_diacritics;
use dotenv;
use env;
use futures::{pin_mut, prelude::Stream};
use reqwest::Client;
use rusqlite::{Connection, *};
use rust_xlsxwriter::Format;
use rust_xlsxwriter::workbook::Workbook;
use scraper::{Element, Html, Selector};
use std::error::Error;
use std::iter::zip;
use std::path::Path;
use std::{collections::HashMap, time::SystemTime};
use time::{Date, format_description::parse_borrowed};
use tokio_stream::StreamExt;

pub struct App {
    pub conn: Connection,
    pub config: Config,
    pub client: Client,
}
#[derive(Debug)]
pub struct Invoice {
    pub invoice_nr: String,
    pub date: Date,
    pub client: String,
    pub sum: i32, //in cents
    pub order_nr: u32,
}

pub struct Payment {
    pub order_nr: Option<u32>,
    pub date: Date,
    pub client: String,
    pub sum: i32, //in cents
}

impl App {
    pub fn new() -> Result<Self, Box<dyn Error>> {
        let conn = Connection::open("app.db3")?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS invoice (
                invoice_nr TEXT PRIMARY KEY,
                date TEXT NOT NULL,
                client INTEGER NOT NULL,
                sum INTEGER NOT NULL,
                order_nr INTEGER ,
                FOREIGN KEY (client) REFERENCES client(client_id)
            ) STRICT;
            CREATE TABLE IF NOT EXISTS client (
                client_id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                normalized_name TEXT NOT NULL UNIQUE
            ) STRICT;
            CREATE TABLE IF NOT EXISTS payment (
                payment_id INTEGER PRIMARY KEY AUTOINCREMENT,
                date TEXT NOT NULL,
                client INTEGER NOT NULL,
                sum INTEGER NOT NULL,
                order_nr INTEGER,
                FOREIGN KEY (client) REFERENCES client(client_id)
            ) STRICT;
            CREATE TABLE IF NOT EXISTS fetch_log (
                log_id INTEGER PRIMARY KEY AUTOINCREMENT,
                invoice_date_to TEXT NOT NULL,
                payment_date_to TEXT NOT NULL,
                completed_at TEXT NOT NULL
            ) STRICT;
        ",
        )?;

        let config = Config::build()?;

        let agent = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));
        let mut client = reqwest::Client::builder()
            .user_agent(agent)
            .cookie_store(true)
            .referer(true);
        if config.proxy {
            client = client.proxy(reqwest::Proxy::http("http://localhost:8080")?);
        }
        let client = client.build()?;

        Ok(App {
            conn,
            config,
            client,
        })
    }

    pub fn get_balance(&self) -> Result<Vec<Customer>, Box<dyn Error>> {
        let mut stmt = self.conn.prepare(
            "WITH invoice_totals AS (
            SELECT client, SUM(sum) AS total_invoiced
            FROM invoice
            WHERE date <= ?1
            GROUP BY client
            ),
            payment_totals AS (
                SELECT client, SUM(sum) AS total_paid
                FROM payment
                WHERE date <= ?2
                GROUP BY client
            )
            SELECT
                c.name AS client_name,
                COALESCE(it.total_invoiced, 0) - COALESCE(pt.total_paid, 0) AS balance_due
            FROM client c
            LEFT JOIN invoice_totals it ON it.client = c.client_id
            LEFT JOIN payment_totals pt ON pt.client = c.client_id
            ORDER BY c.client_id;",
        )?;
        let customer_itr = stmt.query_map(
            params![
                self.config.invoice_date_to.to_string(),
                self.config.payment_date_to.to_string()
            ],
            |row| {
                Ok(Customer {
                    name: row.get(0)?,
                    balance: row.get(1)?,
                })
            },
        )?;
        let mut customers = Vec::new();
        let mut fails = 0;
        for entry in customer_itr {
            if let Ok(customer) = entry {
                customers.push(customer);
            } else {
                fails += 1;
            }
        }
        if fails > 0 {
            println!("Could not map {fails} customers");
        }

        Ok(customers)
    }

    pub fn persist_invoices(&self, invoices: &[Invoice]) -> Result<(), Box<dyn Error>> {
        for invoice in invoices {
            let id = self.persist_client(&invoice.client)?;
            let format = parse_borrowed::<3>("[year]-[month]-[day]").unwrap();
            self.conn.execute(
                "
                    INSERT OR IGNORE INTO invoice VALUES (?1, ?2, ?3, ?4, ?5)
                ",
                params![
                    invoice.invoice_nr,
                    invoice.date.format(&format)?,
                    id,
                    invoice.sum,
                    invoice.order_nr
                ],
            )?;
        }
        Ok(())
    }

    pub fn persist_payments(&self, payments: &[Payment]) -> Result<(), Box<dyn Error>> {
        for payment in payments {
            let id = self.persist_client(&payment.client)?;
            let format = parse_borrowed::<3>("[year]-[month]-[day]").unwrap();
            match payment.order_nr {
                Some(order_nr) => {
                    self.conn.execute(
                        "INSERT OR IGNORE INTO payment (date,client,sum,order_nr) VALUES (?1, ?2, ?3, ?4);",
                        params![payment.date.format(&format)?, id, payment.sum, order_nr],
                    )?;
                }
                None => {
                    self.conn.execute(
                        "INSERT OR IGNORE INTO payment (date,client,sum) VALUES (?1, ?2, ?3);",
                        params![payment.date.format(&format)?, id, payment.sum],
                    )?;
                }
            }
        }
        Ok(())
    }

    fn persist_client(&self, name: &str) -> Result<u32, Box<dyn Error>> {
        let normalized_name = normalize_name(name);
        let id: u32 = self.conn.query_row(
            "
                INSERT INTO client (name, normalized_name) VALUES (?1, ?2)
                ON CONFLICT (normalized_name) DO UPDATE SET normalized_name = excluded.normalized_name
                RETURNING client_id;
            ",
            params![name.to_owned(), normalized_name],
            |row| row.get(0),
        )?;
        Ok(id)
    }

    fn get_latest_payment_date(&self) -> Result<Option<Date>, Box<dyn Error>> {
        if self.config.force {
            return Ok(None);
        }
        let date: Option<String> = self
            .conn
            .query_row(
                "SELECT payment_date_to FROM fetch_log ORDER BY payment_date_to DESC LIMIT 1;",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let format = parse_borrowed::<3>("[year]-[month]-[day]").unwrap();
        let date = date.map(|val| Date::parse(val.as_str(), &format).unwrap());
        Ok(date)
    }

    fn get_latest_invoice_date(&self) -> Result<Option<Date>, Box<dyn Error>> {
        if self.config.force {
            return Ok(None);
        }
        let date: Option<String> = self
            .conn
            .query_row(
                "SELECT invoice_date_to FROM fetch_log ORDER BY invoice_date_to DESC LIMIT 1;",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let format = parse_borrowed::<3>("[year]-[month]-[day]").unwrap();
        let date = date.map(|val| Date::parse(val.as_str(), &format).unwrap());
        Ok(date)
    }

    fn log_query(&self) -> Result<(), Box<dyn Error>> {
        let date = Utc::now();
        let formatted_date = date.format("%Y-%m-%d %H:%M:%S").to_string();
        self.conn.execute("INSERT INTO fetch_log (invoice_date_to, payment_date_to, completed_at) VALUES (?1, ?2, ?3);", params![
            self.config.invoice_date_to.to_string(),
            self.config.payment_date_to.to_string(),
            formatted_date,
        ])?;
        Ok(())
    }
}

pub struct Config {
    pub dry: bool,
    pub proxy: bool,
    pub force: bool,
    pub url: String,
    pub login: String,
    pub password: String,
    pub invoice_date_to: Date,
    pub payment_date_to: Date,
}

impl Config {
    pub fn build() -> Result<Self, Box<dyn Error>> {
        dotenv::dotenv().expect("Could not read the .env file");
        let mut force = false;
        let args = env::args().skip(1);
        let mut positional = Vec::new();
        for arg in args {
            match arg.as_str() {
                "-f" | "--force" => force = true,
                _ => positional.push(arg),
            }
        }

        let invoice_date_to = positional.get(0).ok_or("No invoice date provided")?;
        let payment_date_to = positional.get(1).ok_or("No payment date provided")?;

        let format = parse_borrowed::<3>("[year]-[month]-[day]").unwrap();
        let invoice_date_to = Date::parse(invoice_date_to.as_str(), &format)?;
        let payment_date_to = Date::parse(payment_date_to.as_str(), &format)?;

        Ok(Self {
            dry: env::var("DRY").is_ok(),
            proxy: env::var("PROXY").is_ok(),
            force: force,
            url: env::var("URL").map_err(|_| "URL variable is not set.")?,
            login: env::var("LOGIN").map_err(|_| "LOGIN variable is not set.")?,
            password: env::var("PASSWORD").map_err(|_| "PASSWORD variable is not set.")?,
            invoice_date_to,
            payment_date_to,
        })
    }
}

pub struct Command {
    pub name: String,
    pub description: String,
    pub arguments: Option<Vec<String>>,
}

#[derive(Debug)]
pub struct Customer {
    pub name: String,
    pub balance: i32,
}

pub const HIDDEN_FIELDS: [&'static str; 3] =
    ["__VIEWSTATE", "__VIEWSTATEGENERATOR", "__EVENTVALIDATION"];

pub fn create_order_page_stream(
    client: Client,
    mut params: HashMap<String, String>,
    url: String,
    query: Option<String>,
) -> impl Stream<Item = Result<String, Box<dyn Error>>> {
    try_stream! {
        let page = fetch_debt_page(&client, params.clone(), &url, query.as_deref(), None).await?;
        let html = Html::parse_document(&page);
        yield page;
        params = parse_hidden_fields(&html, &HIDDEN_FIELDS)?;
        let mut page_count = parse_page_count(&html)?;
        let mut current_page = 2;
        loop {
            let page = fetch_debt_page(&client, params.clone(), &url, query.as_deref(), Some(current_page)).await?;
            let html = Html::parse_document(&page);

            yield page;

            if current_page == 11 {
                current_page = 1;
                page_count = parse_page_count(&html)?;
                params = parse_hidden_fields(&html, &HIDDEN_FIELDS)?;
            }
            else if current_page == page_count {
                break;
            }
            current_page += 1;
        }
    }
}

pub async fn run(app: &App) -> Result<(), Box<dyn Error>> {
    login(&app.config, &app.client).await?;
    run_invoices(&app).await?;
    run_payments(&app).await?;
    app.log_query()?;
    Ok(())
}

async fn run_invoices(app: &App) -> Result<(), Box<dyn Error>> {
    let latest_invoice = app.get_latest_invoice_date()?;
    if let Some(date) = latest_invoice
        && app.config.invoice_date_to <= date
    {
        println!("No invoices to fetch");
        return Ok(());
    }
    let page = app
        .client
        .get(format!("{}Reports/InvoiceReport.aspx", &app.config.url))
        .send()
        .await?
        .text()
        .await?;
    let html = Html::parse_document(&page);
    let params = parse_hidden_fields(&html, &HIDDEN_FIELDS)?;

    println!("Fetching invoices");

    let page = fetch_invoice_page(
        &app.client,
        params,
        app.config.invoice_date_to,
        latest_invoice,
        app.config.url.as_str(),
    )
    .await?;
    println!("Parsing invoices");
    let html = Html::parse_document(page.as_str());
    let res = parse_invoice_page(&html);
    app.persist_invoices(&res)?;
    println!("Invoices fetched");
    Ok(())
}

async fn run_payments(app: &App) -> Result<(), Box<dyn Error>> {
    let latest_payment = app.get_latest_payment_date()?;

    if let Some(date) = latest_payment
        && app.config.payment_date_to <= date
    {
        println!("No payments to fetch");
        return Ok(());
    }
    println!("Fetching payments");
    let page = app
        .client
        .get(format!("{}/PaymentsList.aspx", &app.config.url))
        .send()
        .await?
        .text()
        .await?;
    let html = Html::parse_document(&page);
    let params = parse_hidden_fields(&html, &HIDDEN_FIELDS)?;

    let page_stream = create_payments_page_stream(
        &app.client,
        params,
        app.config.url.as_str(),
        &app.config.payment_date_to,
        latest_payment,
    );
    pin_mut!(page_stream);
    let mut count = 1;
    while let Some(res) = page_stream.next().await {
        println!("Parsing page {count}");
        match res {
            Ok(page) => {
                let payments = parse_payments_page(&Html::parse_document(page.as_str()));
                app.persist_payments(&payments)?;
            }
            Err(msg) => println!("Error: {msg}"),
        }
        count += 1;
    }
    println!("Payments fetched");

    Ok(())
}

pub fn create_payments_page_stream(
    client: &Client,
    mut params: HashMap<String, String>,
    url: &str,
    date_to: &Date,
    date_from: Option<Date>,
) -> impl Stream<Item = Result<String, Box<dyn Error>>> {
    try_stream! {
        let page = fetch_payment_page(&client, params.clone(), date_to.clone(), date_from, &url, None).await?;
        let html = Html::parse_document(&page);
        yield page;
        params = parse_hidden_fields(&html, &HIDDEN_FIELDS)?;
        let mut current_page = 2;
        loop {
            let Ok(page) = fetch_payment_page(&client, params.clone(), date_to.clone(), date_from, &url, Some(current_page)).await else {
                break;
            };
            if current_page % 10 == 1{
                let html = Html::parse_document(&page);
                params = parse_hidden_fields(&html, &HIDDEN_FIELDS)?;
            }
            yield page;
            current_page+=1;
        }

    }
}

pub fn export_excel(data: &HashMap<String, f32>, path: &Path) -> Result<(), Box<dyn Error>> {
    let mut workbook = Workbook::new();
    let currency_format = Format::new().set_num_format("[$€-413] #,##0.00;[RED][$€-413] #,##0.00");
    let worksheet = workbook.add_worksheet();
    worksheet.set_column_width(0, 22)?;
    for (i, (client, debt)) in data.iter().enumerate() {
        let i = u32::try_from(i)?;
        worksheet.write(i, 0, client)?;
        worksheet.write_with_format(i, 1, *debt, &currency_format)?;
    }
    workbook.save(path)?;
    Ok(())
}

pub fn parse_page_count(page: &Html) -> Result<u32, Box<dyn Error>> {
    let selector =
        Selector::parse("a[id^=ctl00_MainContent_pagingList_rptPageNumbers_ctl]").unwrap();
    let inner_html = page
        .select(&selector)
        .last()
        .ok_or("No nav elements found")?
        .inner_html();
    if inner_html == "..." {
        return Ok(11);
    }
    let num = inner_html.parse::<u32>()?;
    if num % 10 != 0 { Ok(num % 10) } else { Ok(10) }
}

pub fn parse_debt_page(page: &Html) -> f32 {
    let selector = Selector::parse("td[id$=tdDebt]").unwrap();
    let mut debt = 0f32;
    for el in page.select(&selector) {
        let html = el.inner_html();
        let Some(num) = html.split(" ").next() else {
            continue;
        };

        debt += num.parse::<f32>().unwrap_or(0f32);
    }
    debt
}

pub fn parse_debt_page_named(page: &Html) -> HashMap<String, f32> {
    let mut res = HashMap::new();
    let debt_selector = Selector::parse("td[id$=tdDebt]").unwrap();
    let client_selector = Selector::parse("td[id$=tdClient] > a").unwrap();
    let debt_iterator = page.select(&debt_selector);
    let client_iterator = page.select(&client_selector);
    for (client, debt) in zip(client_iterator, debt_iterator) {
        let debt_html = debt.inner_html();
        let Some(num) = debt_html.split(" ").next() else {
            continue;
        };

        let debt = num.parse::<f32>().unwrap_or(0f32);
        let client = client.inner_html();
        res.entry(client).and_modify(|e| *e += debt).or_insert(debt);
    }
    res
}

pub async fn login(config: &Config, client: &Client) -> Result<String, Box<dyn Error>> {
    let url = format!("{}Login.aspx", &config.url);
    let res = client.get(&url).send().await?.text().await?;

    let body = Html::parse_document(&res);
    let mut params = parse_hidden_fields(&body, &HIDDEN_FIELDS[..3])?;
    params.extend([
        (String::from("txtLogin"), config.login.clone()),
        (String::from("txtPassword"), config.password.clone()),
        (String::from("btnLogin"), String::from("Login")),
    ]);

    Ok(client.post(&url).form(&params).send().await?.text().await?)
}

pub async fn fetch_debt_page(
    client: &Client,
    mut hidden_fields: HashMap<String, String>,
    url: &str,
    name: Option<&str>,
    n: Option<u32>,
) -> Result<String, Box<dyn Error>> {
    if let Some(n) = n {
        let mut btn = "PageNumber";
        if n == 11 {
            btn = "NextPage";
        }
        hidden_fields.insert(
            String::from("__EVENTTARGET"),
            format!(
                "ctl00$MainContent$pagingList$rptPageNumbers$ctl{:02}$btn{btn}",
                n
            ),
        );
    }
    if let Some(name) = name {
        hidden_fields.insert(
            String::from("ctl00$MainContent$txtFilterClient"),
            name.to_owned(),
        );
    }
    let url = format!("{}Default.aspx", &url);
    Ok(client
        .post(&url)
        .form(&hidden_fields)
        .send()
        .await?
        .text()
        .await?)
}

pub fn parse_hidden_fields(
    body: &Html,
    fields: &[&'static str],
) -> Result<HashMap<String, String>, &'static str> {
    let mut result = HashMap::new();
    for &name in fields {
        let selector = String::from("#") + name;
        let selector = Selector::parse(selector.as_str()).unwrap(); //cant fail
        let value = body
            .select(&selector)
            .next()
            .ok_or("Could not find the hidden element")?
            .value()
            .attr("value")
            .ok_or("The hidden element does not have a value")?;

        result.insert(String::from(name), String::from(value));
    }
    Ok(result)
}

pub async fn fetch_invoice_page(
    client: &Client,
    mut hidden_fields: HashMap<String, String>,
    date_to: Date,
    date_from: Option<Date>,
    url: &str,
) -> Result<String, Box<dyn Error>> {
    if let Some(date) = date_from {
        hidden_fields.insert(
            "ctl00$MainContent$txtFilterDateFrom".to_owned(),
            date.to_string(),
        );
    } else {
        hidden_fields.insert(
            "ctl00$MainContent$txtFilterDateFrom".to_owned(),
            "1970-01-01".to_owned(),
        );
    }
    hidden_fields.insert(
        "ctl00$MainContent$txtFilterDateTo".to_owned(),
        date_to.to_string(),
    );
    hidden_fields.insert(
        "ctl00$MainContent$btnFilter".to_owned(),
        "Rodyti".to_owned(),
    );
    let url = format!("{}Reports/InvoiceReport.aspx", &url);
    Ok(client
        .post(&url)
        .form(&hidden_fields)
        .send()
        .await?
        .text()
        .await?)
}

pub async fn fetch_payment_page(
    client: &Client,
    mut hidden_fields: HashMap<String, String>,
    date_to: Date,
    date_from: Option<Date>,
    url: &str,
    page_nr: Option<u32>,
) -> Result<String, Box<dyn Error>> {
    if let Some(date) = date_from {
        hidden_fields.insert(
            "ctl00$MainContent$txtFilterDateFrom".to_owned(),
            date.to_string(),
        );
    } else {
        hidden_fields.insert(
            "ctl00$MainContent$txtFilterDateFrom".to_owned(),
            "1970-01-01".to_owned(),
        );
    }
    hidden_fields.insert(
        "ctl00$MainContent$txtFilterDateTo".to_owned(),
        date_to.to_string(),
    );
    if let Some(n) = page_nr {
        hidden_fields.insert("__EVENTARGUMENT".to_owned(), format!("Page${n}"));
        hidden_fields.insert(
            "__EVENTTARGET".to_owned(),
            "ctl00$MainContent$grdPaymentsList".to_owned(),
        );
    };
    let url = format!("{}/PaymentsList.aspx", &url);

    Ok(client
        .post(&url)
        .form(&hidden_fields)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?)
}

pub fn parse_invoice_page(page: &Html) -> Vec<Invoice> {
    let table_selector = Selector::parse("table.progressDocumentTable").unwrap();
    let tr_selector = Selector::parse("tr").unwrap();
    let td_selector = Selector::parse("td.TableCell").unwrap();
    let table = page.select(&table_selector).next().unwrap();
    let mut res = Vec::new();
    for row in table.select(&tr_selector).skip(8) {
        let mut column_itr = row.select(&td_selector);
        let invoice_nr_elem = column_itr.next().unwrap();

        if invoice_nr_elem
            .value()
            .has_class("Bold", scraper::CaseSensitivity::AsciiCaseInsensitive)
        {
            break;
        }
        let invoice_nr = invoice_nr_elem.text().next().unwrap().to_owned();
        let order_nr = column_itr
            .next()
            .unwrap()
            .inner_html()
            .parse::<u32>()
            .unwrap();
        let date = column_itr.next().unwrap().inner_html();
        let format = parse_borrowed::<3>("[year]-[month]-[day]").unwrap();
        let date = Date::parse(&date, &format).unwrap();
        let client = column_itr.next().unwrap();
        let sum = column_itr.next().unwrap();
        let sum_html = sum.inner_html();
        let Some(num) = sum_html.split_whitespace().next() else {
            continue;
        };
        let sum = num.parse::<f32>().unwrap_or(0f32);
        let sum = (sum * 100f32) as i32;
        let client = client.inner_html();
        let invoice = Invoice {
            invoice_nr: invoice_nr,
            date: date,
            client: client,
            sum: sum,
            order_nr: order_nr,
        };
        res.push(invoice);
    }
    res
}

pub fn parse_payments_page(page: &Html) -> Vec<Payment> {
    let mut res = Vec::new();
    let table_selector = Selector::parse("table[id=ctl00_MainContent_grdPaymentsList]").unwrap();
    let tr_selector = Selector::parse("tr").unwrap();
    let td_selector = Selector::parse("td").unwrap();
    let table = page.select(&table_selector).next().unwrap();
    for row in table.select(&tr_selector).skip(1) {
        if let Some(_) = row.attr("align") {
            //reached navigation rows
            break;
        };
        let mut column_itr = row.select(&td_selector);
        let date = column_itr.next().unwrap().text().nth(1).unwrap();

        let format = parse_borrowed::<3>("[year]-[month]-[day]").unwrap();
        let date = Date::parse(date, &format).unwrap();
        let client = column_itr
            .next()
            .unwrap()
            .text()
            .next()
            .unwrap()
            .trim()
            .to_owned();
        let order_nr = column_itr.next().unwrap().text().next().unwrap().trim();
        let order_nr = order_nr.parse::<u32>().ok();
        let sum = column_itr.nth(2).unwrap().text().next().unwrap().trim();
        let Some(num) = sum.split_whitespace().next() else {
            continue;
        };
        let sum = num.parse::<f32>().unwrap_or(0f32);
        let sum = (sum * 100f32) as i32;
        let payment = Payment {
            order_nr,
            date,
            client,
            sum,
        };
        res.push(payment);
    }
    res
}

pub fn normalize_name(name: &str) -> String {
    let mut res = remove_diacritics(name);
    res = res.to_lowercase();

    let mut prev = ' ';
    res.retain(|c| {
        let result = c != ' ' || prev != ' ';
        prev = c;
        result
    });
    let res = res.trim();
    res.to_owned()
}

pub fn normalize_all_names(conn: &Connection) -> Result<(), Box<dyn Error>> {
    let mut stmt = conn.prepare("SELECT client_id, name FROM client;")?;
    let itr = stmt.query_map([], |row| {
        let id: u32 = row.get(0)?;
        let name: String = row.get(1)?;
        let normalized = normalize_name(&name);
        Ok((id, normalized))
    })?;

    for client in itr {
        println!("{client:?}");
        if let Ok((id, name)) = client {
            conn.execute(
                "UPDATE client SET normalized_name = ?1 WHERE client_id = ?2;",
                [name, id.to_string()],
            )?;
        }
    }

    Ok(())
}
