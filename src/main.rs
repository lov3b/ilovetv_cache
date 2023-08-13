use actix_files::Files;
use actix_web::{App, HttpServer};
use anyhow::{Context, Error, Result};
use chrono::{self, Duration, Local, NaiveTime};
use dotenv;
use futures::TryStreamExt;
use reqwest::{self, Client};
use std::collections::HashMap;
use std::time::Duration as StdDuration;
use std::{env, process};
use strum::{Display, EnumIter, IntoEnumIterator};
use tokio::fs::{self, File};
use tokio::io::AsyncWriteExt;
use tokio::{join, time};

const SERVE_DIR: &'static str = "./ilovetv_cache";
const SERVER_ADDR: &'static str = "127.0.0.1:5050";
const USER_AGENT: &'static str = "ilovetv";

#[tokio::main]
async fn main() -> Result<()> {
    dotenv::dotenv()?;
    let _ = fs::create_dir(SERVE_DIR).await;
    println!("Welcome to ilovetv cache!");
    let ilovetv = ILoveTv::new();
    let ilovetv_daemon = ilovetv.daemonize();

    let http_server =
        HttpServer::new(move || App::new().service(Files::new("/", SERVE_DIR).prefer_utf8(true)))
            .bind(SERVER_ADDR)?
            .run();
    println!("Serving cache on {}", SERVER_ADDR);

    let _ = join!(http_server, ilovetv_daemon);
    Ok(())
}

struct ILoveTv {
    m3u: Box<str>,
    xml_tv: Option<Box<str>>,
    client: Client,
}

impl ILoveTv {
    fn new() -> Self {
        let mut vars = env::vars()
            .map(|(k, v)| (k, v.into_boxed_str()))
            .collect::<HashMap<_, _>>();
        let m3u = if let Some(m3u) = vars.remove("M3U") {
            m3u
        } else {
            eprintln!("$M3U not found");
            process::exit(0);
        };
        let xml_tv = vars.remove("XML_TV");
        if xml_tv.is_none() {
            eprintln!("$XML_TV not found, proceeding without...");
        }

        Self {
            m3u,
            xml_tv,
            client: Client::new(),
        }
    }

    async fn daemonize(&self) {
        let target_time = NaiveTime::from_hms_opt(5, 30, 0).unwrap();
        let current_time = Local::now().time();
        if current_time < NaiveTime::from_hms_opt(19, 00, 00).unwrap() {
            self.refresh_loop(0).await;
        }

        loop {
            let current_time = Local::now().time();
            let duration_till_target = target_time - current_time;

            let sleep_duration = if duration_till_target.num_seconds() >= 0 {
                // If it's before 5:30 AM, use the difference between 5:30 AM and the current time
                duration_till_target
            } else {
                // If it's after 5:30 AM, add 24 hours to get the sleep duration for the next day
                duration_till_target + Duration::days(1)
            };

            println!(
                "Sleeping {}:{}",
                sleep_duration.num_hours(),
                sleep_duration.num_minutes() % 60
            );
            let sleep_duration = StdDuration::new(
                sleep_duration.num_seconds() as u64,
                (sleep_duration.num_nanoseconds().unwrap() % 1_000_000_000) as u32,
            );

            time::sleep(sleep_duration).await;
            self.refresh_loop(10).await;
        }
    }

    async fn refresh_loop(&self, retry: usize) {
        for link_type in LinkType::iter() {
            let link_name = link_type.to_string();
            println!("Downloading {}", &link_name);

            let mut status = self.refresh(&link_type).await;
            let mut counter = 1;
            while status.is_err() && counter <= retry {
                println!(
                    "Failed to download ({}/10) {}, will sleep 30 seconds",
                    counter, &link_name
                );
                time::sleep(StdDuration::from_secs(30)).await;

                status = self.refresh(&link_type).await;
                counter += 1;
            }
        }
    }

    async fn refresh(&self, link_type: &LinkType) -> Result<()> {
        let (link, file_name) = match link_type {
            LinkType::M3U => (Some(&self.m3u), "ilovetv.m3u"),
            LinkType::XmlTv => (self.xml_tv.as_ref(), "xmltv.xml"),
        };
        let link = if let Some(l) = link { l } else { return Ok(()) };

        let (beginning, file_ext) = file_name.rsplit_once('.').context("Malformed filename")?;
        let tmp_file_name = format!("{}-temp.{}", beginning, file_ext);
        println!("save to: {}", format!("{}/{}", SERVE_DIR, &tmp_file_name));

        let status = self
            .save_to_file(link, &format!("{}/{}", SERVE_DIR, &tmp_file_name))
            .await;
        if let Err(e) = status {
            eprintln!("Error occured downloading '{}', {:?}", link, e);
        }

        let name = (
            format!("{}/{}", SERVE_DIR, tmp_file_name),
            format!("{}/{}", SERVE_DIR, file_name),
        );
        println!(
            "No errors on save file, will rename {} to {}",
            &name.0, &name.1
        );
        fs::rename(name.0, name.1).await?;
        println!("Refreshed {}", file_name);

        Ok(())
    }

    async fn save_to_file(&self, link: &str, file_name: &str) -> Result<()> {
        let response = self
            .client
            .get(link)
            .header("User-Agent", USER_AGENT)
            .header("Accept", "*/*")
            .send()
            .await?;

        if !response.status().is_success() {
            println!("Got status code {}", response.status().as_u16());
            return Err(Error::msg(format!(
                "Got status code {} from {}",
                response.status().as_u16(),
                link,
            )));
        }

        let mut f = File::create(file_name).await?;
        println!("Created file");
        let mut stream = response.bytes_stream();
        println!("Got byte stream");

        while let Ok(Some(item)) = stream.try_next().await {
            f.write(&item).await?;
        }

        Ok(())
    }
}

#[derive(EnumIter, Display)]
enum LinkType {
    M3U,
    XmlTv,
}
