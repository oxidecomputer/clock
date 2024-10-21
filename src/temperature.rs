use std::{
    collections::HashMap,
    str::FromStr,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{anyhow, bail, Result};
use prometheus_http_query::{
    response::{Data, PromqlResult},
    Client,
};
use slog::{error, Logger};

pub struct Temperatures {
    inner: Arc<Inner>,
}

struct Inner {
    log: Logger,
    temps: Mutex<HashMap<String, f64>>,
}

impl Temperatures {
    pub fn new(log: Logger) -> Result<Temperatures> {
        let prom =
            Client::from_str("http://catacomb.eng.oxide.computer:9090/")?;
        let inner: Arc<Inner> =
            Arc::new(Inner { log, temps: Default::default() });

        {
            let inner = Arc::clone(&inner);
            tokio::task::spawn(async move {
                temperature_noerr(&inner, &prom).await;
            });
        }

        Ok(Temperatures { inner })
    }

    pub fn temperatures(&self, names: &[&str]) -> Vec<Option<f64>> {
        let l = self.inner.temps.lock().unwrap();

        names.iter().map(|n| l.get(*n).copied()).collect()
    }
}

async fn temperature_noerr(inner: &Inner, prom: &Client) {
    loop {
        if let Err(e) = temperature(inner, prom).await {
            error!(&inner.log, "temperature fetch error: {e}");
        }

        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn temperature(inner: &Inner, prom: &Client) -> Result<()> {
    let q = "(temperature_degrees_celsius * (9/5)) + 32";

    let temps = prom
        .query(q)
        .get()
        .await?
        .into_inner()
        .0
        .into_vector()
        .map_err(|_| anyhow!("result was not a vector?"))?
        .into_iter()
        .filter_map(|d| {
            if let Some(loc) = d.metric().get("location") {
                Some((loc.to_string(), d.sample().value()))
            } else {
                None
            }
        })
        .collect();

    *inner.temps.lock().unwrap() = temps;

    println!("temps = {:#?}", inner.temps.lock().unwrap());

    Ok(())
}
