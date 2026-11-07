//! Opens the object store named by `STORE_URL`.

use crate::config::Config;
use anyhow::{Context, bail};
use object_store::ObjectStore;
use object_store::aws::AmazonS3Builder;
use object_store::local::LocalFileSystem;
use object_store::memory::InMemory;
use object_store::prefix::PrefixStore;
use std::sync::Arc;

pub fn open(cfg: &Config) -> anyhow::Result<Arc<dyn ObjectStore>> {
    let url = cfg.store_url.as_str();
    if let Some(path) = url.strip_prefix("file://") {
        // Accept relative paths (`file://./data/store`), which a strict URL
        // parser would not.
        std::fs::create_dir_all(path)
            .with_context(|| format!("creating the tile store directory {path:?}"))?;
        let abs = std::fs::canonicalize(path)?;
        // Without cleanup every expired frame would leave its empty
        // directories behind, and listing the store would slow down forever.
        let fs = LocalFileSystem::new_with_prefix(abs)?.with_automatic_cleanup(true);
        return Ok(Arc::new(fs));
    }
    if url == "memory://" {
        return Ok(Arc::new(InMemory::new()));
    }
    if let Some(rest) = url.strip_prefix("s3://") {
        let (bucket, prefix) = match rest.split_once('/') {
            Some((b, p)) => (b, p.trim_matches('/')),
            None => (rest, ""),
        };
        if bucket.is_empty() {
            bail!("STORE_URL {url:?} has no bucket name");
        }
        let mut b = AmazonS3Builder::new()
            .with_bucket_name(bucket)
            // R2 and most S3-compatible services ignore the region but the
            // signature needs one.
            .with_region(cfg.s3.region.as_deref().unwrap_or("auto"));
        if let Some(endpoint) = &cfg.s3.endpoint {
            b = b
                .with_endpoint(endpoint)
                .with_virtual_hosted_style_request(false);
        }
        match (&cfg.s3.access_key_id, &cfg.s3.secret_access_key) {
            (Some(id), Some(secret)) => {
                b = b.with_access_key_id(id).with_secret_access_key(secret);
            }
            _ => {
                bail!("STORE_URL is s3:// but S3_ACCESS_KEY_ID / S3_SECRET_ACCESS_KEY are not set")
            }
        }
        let s3 = b.build().context("configuring the S3 tile store")?;
        return Ok(if prefix.is_empty() {
            Arc::new(s3)
        } else {
            Arc::new(PrefixStore::new(s3, prefix))
        });
    }
    bail!(
        "STORE_URL {url:?} is not supported; use file://<dir>, s3://<bucket>[/prefix] or memory://"
    )
}
