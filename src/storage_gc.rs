use native_tls::TlsConnector;
use postgres_native_tls::MakeTlsConnector;
use std::env;
use std::time::Duration;
use tokio_postgres::Client;

const AUTO_CATEGORIES: &[&str] = &["site_article", "site_headline", "discovery", "user_history"];
const MAX_AUTO_ROWS_PER_DB: i64 = 25_000;
const RETENTION_DAYS: i64 = 14;
const GC_INTERVAL_SECS: u64 = 6 * 60 * 60;

async fn connect(url: &str) -> Result<Client, String> {
    let config = url.parse::<tokio_postgres::Config>().map_err(|e| e.to_string())?;
    let mut builder = TlsConnector::builder();
    builder.danger_accept_invalid_certs(true);
    let tls = MakeTlsConnector::new(builder.build().map_err(|e| e.to_string())?);
    let (client, connection) = config.connect(tls).await.map_err(|e| e.to_string())?;

    tokio::spawn(async move {
        if let Err(err) = connection.await {
            eprintln!("[Storage GC] Postgres connection closed: {err}");
        }
    });

    Ok(client)
}

async fn clean_database(url: &str, label: &str) {
    let client = match connect(url).await {
        Ok(client) => client,
        Err(err) => {
            eprintln!("[Storage GC] {label}: connection failed: {err}");
            return;
        }
    };

    let categories = AUTO_CATEGORIES
        .iter()
        .map(|category| format!("'{category}'"))
        .collect::<Vec<_>>()
        .join(",");

    let cutoff = format!(
        "EXTRACT(EPOCH FROM (NOW() - INTERVAL '{} days'))::BIGINT",
        RETENTION_DAYS
    );

    let stale_sql = format!(
        "DELETE FROM human_learning_vectors WHERE category IN ({categories}) AND last_updated < {cutoff}"
    );

    match client.execute(stale_sql.as_str(), &[]).await {
        Ok(count) => println!("[Storage GC] {label}: removed {count} stale learning rows"),
        Err(err) => eprintln!("[Storage GC] {label}: retention cleanup skipped: {err}"),
    }

    let cap_sql = format!(
        "WITH overflow AS (\n             SELECT id FROM human_learning_vectors\n             WHERE category IN ({categories})\n             ORDER BY last_updated DESC\n             OFFSET {MAX_AUTO_ROWS_PER_DB}\n         )\n         DELETE FROM human_learning_vectors WHERE id IN (SELECT id FROM overflow)"
    );

    match client.execute(cap_sql.as_str(), &[]).await {
        Ok(count) if count > 0 => println!("[Storage GC] {label}: removed {count} rows above the {MAX_AUTO_ROWS_PER_DB}-row cap"),
        Ok(_) => {}
        Err(err) => eprintln!("[Storage GC] {label}: row-cap cleanup skipped: {err}"),
    }
}

#[tokio::main]
async fn main() {
    let urls = (1..=6)
        .filter_map(|index| env::var(format!("NEON_DB_{index}")).ok())
        .filter(|url| !url.trim().is_empty())
        .collect::<Vec<_>>();

    if urls.is_empty() {
        eprintln!("[Storage GC] No NEON_DB_1..NEON_DB_6 variables configured; nothing to clean.");
        return;
    }

    loop {
        for (index, url) in urls.iter().enumerate() {
            clean_database(url, &format!("NEON_DB_{}", index + 1)).await;
        }

        tokio::time::sleep(Duration::from_secs(GC_INTERVAL_SECS)).await;
    }
}
