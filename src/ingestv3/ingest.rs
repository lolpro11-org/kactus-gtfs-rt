use futures::prelude::*;
use kactus::AgencyInfo;
use rand::Rng;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_zookeeper::*;


#[tokio::main]
async fn main() {
    let mut rng = rand::thread_rng();
    let worker_uid: Arc<u64> = Arc::new(rng.gen::<u64>());

    let (zk, default_watcher) = ZooKeeper::connect(&"127.0.0.1:2181".parse().unwrap())
        .await
        .unwrap();

    let feed_list: Arc<Mutex<Vec<AgencyInfo>>> = Arc::new(Mutex::new(vec![]));

    //wrap zk into an arc to be accessed across several threads
    let zk = Arc::new(zk);

    let check_pool_made = zk.watch().exists("/kactusworkers").await.unwrap();

    if check_pool_made.is_none() {
        let makeworkerpool = zk
            .create(
                "/kactusworkers",
                &b"Hello world"[..],
                Acl::open_unsafe(),
                CreateMode::Persistent,
            )
            .await
            .unwrap();

        let check_pool_made = zk.watch().exists("/kactusworkers").await.unwrap().unwrap();
    }

    let make_worker = zk
        .create(
            format!("/kactusworkers/{worker_uid}").as_str(),
            &b"Hello world"[..],
            Acl::open_unsafe(),
            CreateMode::Ephemeral,
        )
        .await
        .unwrap();

    //seperate leader thread
    tokio::spawn({
        let worker_uid = worker_uid.clone();

        async move {
            loop {
                //if no leader, wait a random amount of time and attempt to become leader
                println!("{worker_uid}");
            }
        }
    });

    println!("{worker_uid}");

    //seperate ingest thread, which is controlled through the feedlist arc mutex above

    //listen handler changes global state
}
