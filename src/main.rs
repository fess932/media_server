mod server;

use anyhow::Context;
use async_recursion::async_recursion;
use axum::http::Method;
use axum::routing::get;
use axum::{Extension, Router};
use dotenvy::dotenv;
use sqlx::error::ErrorKind::UniqueViolation;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Pool, Sqlite};
use std::env;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering::Relaxed;
use std::time::SystemTime;
use tokio::net::TcpListener;
use tower_http::cors::{Any, CorsLayer};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    dotenv().expect(".env file not found");
    let dburl = env::var("DATABASE_URL").expect("wtf not found config DATABASE_URL");
    let rootdir = env::var("ROOT_DIR").expect("wtf not found config ROOT_DIR");

    let cors = CorsLayer::new()
        // allow `GET` and `POST` when accessing the resource
        .allow_methods([Method::GET, Method::POST])
        // allow requests from any origin
        .allow_origin(Any);

    let sqlpool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(dburl.as_str())
        .await
        .context(format!("could not connect to database url {}", dburl))
        .unwrap();
    let pool = sqlpool.clone();
    // sqlx::migrate!("./migrations").run(&sqlpool).await.unwrap();

    let app = Router::new()
        .route("/", get(server::root))
        .route("/dirs", get(server::dirs))
        .nest(
            "/static",
            Router::new().route("/*path", get(server::file_handler)),
        )
        .layer(Extension(sqlpool))
        .layer(Extension(rootdir.clone()))
        .layer(cors);

    let mut ds = DirScanner {
        pool,
        n: AtomicU32::new(0),
    };

    let t = SystemTime::now();
    ds.init(rootdir.clone().to_string().as_str()).await;
    ds.scan(rootdir.clone().to_string().as_str()).await;
    println!(
        "duration {:?}",
        SystemTime::now().duration_since(t).expect("wtf")
    );

    // run our app with hyper
    // `axum::Server` is a re-export of `hyper::Server`
    let listener = TcpListener::bind("0.0.0.0:3100").await.unwrap();
    tracing::debug!("listening on {}", "0.0.0.0:3100");
    println!("server running at :3100");
    axum::serve(listener, app.into_make_service())
        .await
        .unwrap();
}

struct DirScanner {
    pool: Pool<Sqlite>,
    n: AtomicU32,
}

impl DirScanner {
    async fn write_file_to_db(
        &mut self,
        path: &str,
        name: &str,
        is_dir: bool,
        parent: Option<&str>,
    ) {
        let mut file_type = "file";
        if is_dir {
            file_type = "dir"
        }
        let parent_path = parent.unwrap_or("root");

        // println!(
        //     "type {} path {} \n parent_path {}",
        //     file_type, path, parent_path,
        // );

        let result = sqlx::query(
            "insert into files (path, parent_path, name, type) values ($1, $2, $3, $4)",
        )
        .bind(path)
        .bind(parent_path)
        .bind(name)
        .bind(file_type)
        .execute(&self.pool)
        .await;

        if let Err(err) = result {
            if UniqueViolation == err.as_database_error().expect("wtf").kind() {
                // println!("already exists err: {} {}", err, path)
            }
        };
    }

    async fn init(&mut self, path: &str) {
        // println!("root dir {}", path);
        self.write_file_to_db(path, "root", true, None).await;
    }

    #[async_recursion]
    async fn scan(&mut self, path: &str) {
        let mut dir = tokio::fs::read_dir(path).await.expect("wtf");

        while let Some(entry) = dir.next_entry().await.unwrap() {
            if self.n.load(Relaxed) > 6000 {
                return;
            }
            self.n.fetch_add(1, Relaxed);

            let file_type = entry.file_type().await.expect("wtf");
            if file_type.is_file() {
                // println!(
                //     "path {} name {} is file {}, path_dir {}",
                //     entry.path().to_str().expect("wtf"),
                //     entry.file_name().to_str().expect("wtf"),
                //     entry.metadata().await.expect("wtf").is_file(),
                //     path,
                // );

                self.write_file_to_db(
                    entry.path().to_str().expect("wtf"),
                    entry.file_name().to_str().expect("wtf"),
                    false,
                    Some(path),
                )
                .await
            }
            if file_type.is_dir() {
                // println!("dir {}", entry.file_name().to_str().expect("wtf"));
                self.write_file_to_db(
                    entry.path().to_str().expect("wtf"),
                    entry.file_name().to_str().expect("wtf"),
                    true,
                    Some(path),
                )
                .await;
                self.scan(entry.path().to_str().expect("wtf")).await
            }
        }
    }
}
