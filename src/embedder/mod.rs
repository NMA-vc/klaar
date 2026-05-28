use anyhow::Result;
use tracing::{info, warn};

#[cfg(feature = "semantic-search")]
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

#[cfg(feature = "semantic-search")]
/// Wraps a fastembed TextEmbedding model.
/// Initialize once at startup; share via `Arc<Embedder>`.
pub struct Embedder {
    model: TextEmbedding,
}

#[cfg(feature = "semantic-search")]
impl Embedder {
    /// Load BGESmallENV15 (~133 MB, downloaded once to ~/.cache/fastembed/).
    /// Returns `Err` if the model cannot be loaded — callers fall back to BM25.
    pub fn init() -> Result<Self> {
        info!("Loading embedding model BGESmallENV15 (first run downloads ~133 MB)...");
        let model = TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::BGESmallENV15).with_show_download_progress(true),
        )?;
        info!("Embedding model ready.");
        Ok(Self { model })
    }

    /// Attempt to initialize without logging a hard error on failure.
    /// Returns `Some(Embedder)` on success, `None` on any failure (network,
    /// disk space, etc.). Callers degrade gracefully to BM25.
    pub fn try_init() -> Option<Self> {
        match Self::init() {
            Ok(e) => Some(e),
            Err(err) => {
                warn!(
                    "Embedding model failed to load — recall_memory will use BM25 only. \
                     Error: {}",
                    err
                );
                None
            }
        }
    }

    /// Embed a single string into a 384-dimensional float vector.
    pub fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let mut batches = self.model.embed(vec![text.to_string()], None)?;
        Ok(batches.pop().unwrap_or_default())
    }
}

// ---------------------------------------------------------------------------
// Fallback mock when semantic-search is disabled
// ---------------------------------------------------------------------------

#[cfg(not(feature = "semantic-search"))]
pub struct Embedder {}

#[cfg(not(feature = "semantic-search"))]
impl Embedder {
    pub fn try_init() -> Option<Self> {
        warn!("klaar built without 'semantic-search' feature. Falling back to BM25.");
        None
    }

    pub fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        anyhow::bail!("Semantic search is disabled in this build.")
    }
}
