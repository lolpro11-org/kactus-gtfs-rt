use std::{collections::HashMap, fs::File, io::BufReader, sync::{mpsc::{self, Receiver, TryRecvError}, Arc, Mutex}, thread::{self, sleep}, time::Duration};

use kactus::{fetchurl, insert::insert_gtfs_rt_bytes, make_url, parse_protobuf_message, AgencyInfo, Agencyurls};
use rand::seq::SliceRandom;
use reqwest::Client;

use futures::{future, prelude::*};
use tarpc::{
    client, context, server::{incoming::Incoming, BaseChannel}, tokio_serde::formats::Json
};
use tokio::task::{self, JoinHandle};

#[tarpc::service]
pub trait IngestInfo {
    async fn agencies() -> String;
    async fn addagency(agency: AgencyInfo) -> String;
}
#[derive(Clone)]
struct KactusRPC {
    client: Arc<Client>,
    redis_client: Arc<redis::Client>,
    agencies: Arc<Mutex<Vec<AgencyInfo>>>,
    threads: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
}

#[tarpc::server]
impl IngestInfo for KactusRPC {
    async fn agencies(self, _: context::Context) -> String {
        serde_json::to_string(&self.agencies).expect("Failed to serialize to JSON")
    }
    async fn addagency(self, _: context::Context, agency: AgencyInfo) -> String {
        //addtolist(self, agency);
        if !self.agencies.lock().unwrap().contains(&agency) {
            let key = agency.onetrip.clone();
            self.agencies.lock().unwrap().push(agency.clone());
            //self.agencies.lock().unwrap().thread
            let shared_client_clone = Arc::clone(&self.client);
            let redis_client_clone = Arc::clone(&self.redis_client);
            let (tx, rx) = mpsc::channel::<Option<u8>>();
            let handle = tokio::spawn(async move {
                let client = shared_client_clone;
                let redis_client = redis_client_clone;
                fetchagency(&client, &redis_client, agency, rx).await;
            });
            self.threads.lock().unwrap().insert(key, handle);
            return "Agency added".to_string();
        }
        return "Error: Agency Exists".to_string();
    }
}


async fn fetchagency(client: &Client, redis_client: &redis::Client, agency: AgencyInfo, rx: Receiver<Option<u8>>)  {
    //let client = reqwest::ClientBuilder::new().deflate(true).gzip(true).brotli(true).build().unwrap();
    let mut con = redis_client.get_connection().unwrap();
    loop {
        match rx.try_recv() {
            Ok(_) | Err(TryRecvError::Disconnected) => {
                println!("Terminating.");
                break;
            }
            Err(TryRecvError::Empty) => {}
        }
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

        let fetch_vehicles = {
            fetchurl(
                &fetch.vehicles,
                &agency.auth_header,
                &agency.auth_type,
                &passwordtouse,
                &client,
                15_000,
                //timeoutforfetch,
            )
        };
        
        let fetch_trips = {
            fetchurl(
                &fetch.trips,
                &agency.auth_header,
                &agency.auth_type,
                &passwordtouse,
                &client,
                15_000,
                //timeoutforfetch,
            )
        };
        
        let fetch_alerts = {
            fetchurl(
                &fetch.alerts,
                &agency.auth_header,
                &agency.auth_type,
                &passwordtouse,
                &client,
                15_000,
                //timeoutforfetch,
            )
        };
        
        let vehicles_result = fetch_vehicles.await;
        let trips_result = fetch_trips.await;
        let alerts_result = fetch_alerts.await;


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
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {

    let arguments = std::env::args();
    let arguments = arguments::parse(arguments).unwrap();

    let filename = match arguments.get::<String>("urls") {
        Some(filename) => filename,
        None => String::from("urls.csv"),
    };

    let timeoutforfetch = match arguments.get::<u64>("timeout") {
        Some(filename) => filename,
        None => 15_000,
    };

    let file = File::open(filename).unwrap();

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
                    },
                };

                agencies.push(agency);
            }
            Err(e) => {
                println!("error reading csv");
                println!("{:?}", e);
            }
        }
    }

    let shared_client = Arc::new(reqwest::ClientBuilder::new()
        .deflate(true)
        .gzip(true)
        .brotli(true)
        .build()
        .unwrap()
    );

    let redisclient = Arc::new(redis::Client::open("redis://127.0.0.1:6379/").unwrap());

    let mut handles = HashMap::new();

    let base = KactusRPC {
        client: shared_client.clone(),
        redis_client: redisclient.clone(),
        agencies: Arc::new(Mutex::new(Vec::new())), 
        threads: Arc::new(Mutex::new(HashMap::new())), 
    };

    let listener = tarpc::serde_transport::tcp::listen("localhost:9010", Json::default)
        .await?
        .filter_map(|r| future::ready(r.ok()));
    let server = listener
        .map(BaseChannel::with_defaults)
        .execute(base.serve());
    let j = task::spawn(server);

    for agency in agencies.into_iter() {
        let key = agency.onetrip.clone();
        let shared_client_clone = Arc::clone(&shared_client);
        let redis_client_clone = Arc::clone(&redisclient);
        let (tx, rx) = mpsc::channel();
        let handle = tokio::spawn(async move {
            let client = shared_client_clone;
            let redis_client = redis_client_clone;
            fetchagency(&client, &redis_client, agency, rx).await;
        });

        handles.insert(key, handle);
    }

    /*let connection =
        tarpc::serde_transport::tcp::connect("localhost:9010", Json::default).await?;
    let client = IngestInfoClient::new(client::Config::default(), connection).spawn();

    let ctx = context::current();

    println!("{:?}", client.agencies(ctx).await?);*/
    for (index, handle) in handles {
        handle.await.unwrap();
    }
    println!("Ready at localhost");
    j.await?;
    Ok(())
}