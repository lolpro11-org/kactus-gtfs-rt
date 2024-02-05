use futures::join;
use futures::StreamExt;
use kactus::fetchurl;
use kactus::make_url;
use reqwest::Client as ReqwestClient;
use std::time::{Duration, Instant};
use termion::{color, style};
extern crate color_eyre;
use kactus::parse_protobuf_message;
extern crate rand;
use crate::rand::prelude::SliceRandom;
use kactus::insert::insert_gtfs_rt_bytes;
extern crate csv;
use kactus::aspen;
use std::fs::File;
use std::io::BufReader;
use kactus::AgencyInfo;
#[derive(Debug)]
struct Agencyurls {
    vehicles: Option<String>,
    trips: Option<String>,
    alerts: Option<String>,
}

#[tokio::main]
async fn main() -> color_eyre::eyre::Result<()> {
    color_eyre::install()?;

    let arguments = std::env::args();
    let arguments = arguments::parse(arguments).unwrap();

    let filenametouse = match arguments.get::<String>("urls") {
        Some(filename) => filename,
        None => String::from("urls.csv"),
    };

    let timeoutforfetch = match arguments.get::<u64>("timeout") {
        Some(filename) => filename,
        None => 15_000,
    };

    let threadcount = match arguments.get::<usize>("threads") {
        Some(threadcount) => threadcount,
        None => 50,
    };

    let file = File::open(filenametouse).unwrap();
    let mut reader = csv::Reader::from_reader(BufReader::new(file));

    let mut agencies: Vec<AgencyInfo> = Vec::new();

    for record in reader.records() {
        match record {
            Ok(record) => {
                let agency = AgencyInfo {
                    onetrip: record[0].to_string(),
                    realtime_vehicle_positions: record[1].to_string(),
                    realtime_trip_updates: record[2].to_string(),
                    realtime_alerts: record[3].to_string(),
                    has_auth: record[4].parse().unwrap(),
                    auth_type: record[5].to_string(),
                    auth_header: record[6].to_string(),
                    auth_password: record[7].to_string(),
                    fetch_interval: record[8].parse().unwrap(),
                    multiauth: {
                        if !record[9].to_string().is_empty() {
                            let mut outputvec: Vec<String> = Vec::new();
                            for s in record[9].to_string().clone().split(",") {
                                outputvec.push(s.to_string());
                            }
                    
                            Some(outputvec)
                        } else {
                            None
                        }
                    }
                };

                agencies.push(agency);
            }
            Err(e) => {
                println!("error reading csv");
                println!("{:?}", e);
            }
        }
    }

    let mut lastloop;


    let client = reqwest::ClientBuilder::new()
        .deflate(true)
        .gzip(true)
        .brotli(true)
        .build()
        .unwrap();

    loop {

        lastloop = Instant::now();

        let reqquery_vec_cloned = agencies.clone();

        let fetches = futures::stream::iter(reqquery_vec_cloned.into_iter().map(|agency| {
            let client = &client;

            async move {
                let redisclient = redis::Client::open("redis://127.0.0.1:6379/").unwrap();
                let mut con = redisclient.get_connection().unwrap();

                //println!("{:#?}", agency);

                let fetch = Agencyurls {
                    vehicles: make_url(
                        &agency.realtime_vehicle_positions,
                        &agency.auth_type,
                        &agency.auth_header,
                        &agency.auth_password,
                    ),
                    trips: make_url(
                        &agency.realtime_trip_updates,
                        &agency.auth_type,
                        &agency.auth_header,
                        &agency.auth_password,
                    ),
                    alerts: make_url(
                        &agency.realtime_alerts,
                        &agency.auth_type,
                        &agency.auth_header,
                        &agency.auth_password,
                    ),
                };

                let passwordtouse = match &agency.multiauth {
                    Some(multiauth) => {
                        let mut rng = rand::thread_rng();
                        let random_auth = multiauth.choose(&mut rng).unwrap();

                        random_auth.to_string()
                    }
                    None => agency.auth_password.clone(),
                };

                let grouped_fetch = join!(
                    fetchurl(
                        &fetch.vehicles,
                        &agency.auth_header,
                        &agency.auth_type,
                        &passwordtouse,
                        &client,
                        timeoutforfetch,
                    ),
                    fetchurl(
                        &fetch.trips,
                        &agency.auth_header,
                        &agency.auth_type,
                        &passwordtouse,
                        &client,
                        timeoutforfetch,
                    ),
                    fetchurl(
                        &fetch.alerts,
                        &agency.auth_header,
                        &agency.auth_type,
                        &passwordtouse,
                        &client,
                        timeoutforfetch,
                    )
                );

                let vehicles_result = grouped_fetch.0;
                let trips_result = grouped_fetch.1;
                let alerts_result = grouped_fetch.2;

                if vehicles_result.is_some() {
                    let bytes = vehicles_result.as_ref().unwrap().to_vec();

                    println!("{} vehicles bytes: {}", &agency.onetrip, bytes.len());

                    match agency.onetrip.as_str() == "f-octa~rt" {
                        true => {
                            let swiftly_vehicles = parse_protobuf_message(&bytes)
                                .unwrap();
                            let octa_raw_file = client.get("https://api.octa.net/GTFSRealTime/protoBuf/VehiclePositions.aspx").send().await;
                            match octa_raw_file {
                                Ok(octa_raw_file) => {
                                    let octa_raw_file = octa_raw_file.bytes().await.unwrap();
                                    let octa_vehicles = parse_protobuf_message(&octa_raw_file).unwrap();
                                    let mut output_joined = swiftly_vehicles.clone();
                                    insert_gtfs_rt_bytes(
                                        &mut con,
                                        &bytes.to_vec(),
                                        &("f-octa~rt".to_string()),
                                        &("vehicles".to_string()),
                                    );
                                }
                                Err(e) => {
                                    println!("error fetching raw octa file: {:?}", e);
                                    insert_gtfs_rt_bytes(
                                        &mut con,
                                        &bytes.to_vec(),
                                        &("f-octa~rt".to_string()),
                                        &("vehicles".to_string()),
                                    );
                                }
                            }
                        }
                        false => {
                            insert_gtfs_rt_bytes(
                                &mut con,
                                &bytes,
                                &agency.onetrip,
                                &("vehicles".to_string()),
                            );
                        }
                    }   
                }

                if trips_result.is_some() {
                    let bytes = trips_result.as_ref().unwrap().to_vec();

                    println!("{} trips bytes: {}", &agency.onetrip, bytes.len());

                    insert_gtfs_rt_bytes(&mut con, &bytes, &agency.onetrip, &("trips".to_string()));
                }

                if alerts_result.is_some() {
                    let bytes = alerts_result.as_ref().unwrap().to_vec();

                    println!("{} alerts bytes: {}", &agency.onetrip, bytes.len());

                    insert_gtfs_rt_bytes(
                        &mut con,
                        &bytes,
                        &agency.onetrip,
                        &("alerts".to_string()),
                    );
                }

                aspen::send_to_aspen(
                    &agency.onetrip,
                    &vehicles_result,
                    &trips_result,
                    &alerts_result,
                    fetch.vehicles.is_some(),
                    fetch.trips.is_some(),
                    fetch.alerts.is_some(),
                    true,
                )
                .await;
            }
        }))
        .buffer_unordered(threadcount)
        .collect::<Vec<()>>();
        println!("Starting loop: {} fetches", &agencies.len());
        fetches.await;

        let duration = lastloop.elapsed();

        println!(
            "{}loop time is: {:?}{}",
            color::Bg(color::Green),
            duration,
            style::Reset
        );

        //if the iteration of the loop took <0.5 second, sleep for the remainder of the second
        if (duration.as_millis() as i32) < 500 {
            let sleep_duration = Duration::from_millis(500) - duration;
            println!("sleeping for {:?}", sleep_duration);
            std::thread::sleep(sleep_duration);
        }
    }
}

