use anyhow::{bail, Context, Result};
use indexmap::IndexSet;
use serde::{Deserialize, Serialize};
use turbo_tasks::{
    graph::{AdjacencyMap, GraphTraversal},
    trace::TraceRawVcs,
    TaskInput, TryJoinIterExt, Value, Vc,
};
use turbo_tasks_fs::FileSystemPath;
use turbopack_core::{
    chunk::{Chunk, ChunkableModule, ChunkingContext, Chunks, EvaluatableAssets},
    environment::Environment,
    ident::AssetIdent,
    module::Module,
    output::{OutputAsset, OutputAssets},
};
use turbopack_css::chunk::CssChunk;
use turbopack_ecmascript::chunk::{
    EcmascriptChunk, EcmascriptChunkPlaceable, EcmascriptChunkingContext,
};
use turbopack_ecmascript_runtime::RuntimeType;

use crate::ecmascript::node::{
    chunk::EcmascriptBuildNodeChunk, entry::chunk::EcmascriptBuildNodeEntryChunk,
};

#[derive(
    Debug,
    Default,
    TaskInput,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    TraceRawVcs,
)]
pub enum MinifyType {
    #[default]
    Minify,
    NoMinify,
}

/// A builder for [`Vc<BuildChunkingContext>`].
pub struct BuildChunkingContextBuilder {
    chunking_context: BuildChunkingContext,
}

impl BuildChunkingContextBuilder {
    pub fn asset_prefix(mut self, asset_prefix: Vc<Option<String>>) -> Self {
        self.chunking_context.asset_prefix = asset_prefix;
        self
    }

    pub fn minify_type(mut self, minify_type: MinifyType) -> Self {
        self.chunking_context.minify_type = minify_type;
        self
    }

    pub fn runtime_type(mut self, runtime_type: RuntimeType) -> Self {
        self.chunking_context.runtime_type = runtime_type;
        self
    }

    pub fn layer(mut self, layer: impl Into<String>) -> Self {
        self.chunking_context.layer = Some(layer.into());
        self
    }

    /// Builds the chunking context.
    pub fn build(self) -> Vc<BuildChunkingContext> {
        BuildChunkingContext::new(Value::new(self.chunking_context))
    }
}

/// A chunking context for build mode.
#[turbo_tasks::value(serialization = "auto_for_input")]
#[derive(Debug, Clone, Hash, PartialOrd, Ord)]
pub struct BuildChunkingContext {
    /// This path get stripped off of chunk paths before generating output asset
    /// paths.
    context_path: Vc<FileSystemPath>,
    /// This path is used to compute the url to request chunks or assets from
    output_root: Vc<FileSystemPath>,
    /// This path is used to compute the url to request chunks or assets from
    client_root: Vc<FileSystemPath>,
    /// Chunks are placed at this path
    chunk_root_path: Vc<FileSystemPath>,
    /// Static assets are placed at this path
    asset_root_path: Vc<FileSystemPath>,
    /// Static assets requested from this url base
    asset_prefix: Vc<Option<String>>,
    /// Layer name within this context
    layer: Option<String>,
    /// The environment chunks will be evaluated in.
    environment: Vc<Environment>,
    /// The kind of runtime to include in the output.
    runtime_type: RuntimeType,
    /// Whether to minify resulting chunks
    minify_type: MinifyType,
}

impl BuildChunkingContext {
    /// Creates a new chunking context builder.
    pub fn builder(
        context_path: Vc<FileSystemPath>,
        output_root: Vc<FileSystemPath>,
        client_root: Vc<FileSystemPath>,
        chunk_root_path: Vc<FileSystemPath>,
        asset_root_path: Vc<FileSystemPath>,
        environment: Vc<Environment>,
    ) -> BuildChunkingContextBuilder {
        BuildChunkingContextBuilder {
            chunking_context: BuildChunkingContext {
                context_path,
                output_root,
                client_root,
                chunk_root_path,
                asset_root_path,
                asset_prefix: Default::default(),
                layer: None,
                environment,
                runtime_type: Default::default(),
                minify_type: MinifyType::Minify,
            },
        }
    }
}

impl BuildChunkingContext {
    /// Returns the kind of runtime to include in output chunks.
    ///
    /// This is defined directly on `BuildChunkingContext` so it is zero-cost
    /// when `RuntimeType` has a single variant.
    pub fn runtime_type(&self) -> RuntimeType {
        self.runtime_type
    }

    pub fn minify_type(&self) -> MinifyType {
        self.minify_type
    }
}

#[turbo_tasks::value_impl]
impl BuildChunkingContext {
    #[turbo_tasks::function]
    fn new(this: Value<BuildChunkingContext>) -> Vc<Self> {
        this.into_value().cell()
    }

    /// Generates an output chunk that:
    /// * evaluates the given assets; and
    /// * exports the result of evaluating the given module as a CommonJS
    ///   default export.
    #[turbo_tasks::function]
    pub async fn entry_chunk(
        self: Vc<Self>,
        path: Vc<FileSystemPath>,
        module: Vc<Box<dyn EcmascriptChunkPlaceable>>,
        evaluatable_assets: Vc<EvaluatableAssets>,
    ) -> Result<Vc<Box<dyn OutputAsset>>> {
        let entry_chunk = module.as_root_chunk(Vc::upcast(self));

        let other_chunks = self
            .get_chunk_assets(entry_chunk, evaluatable_assets)
            .await?;

        let asset = Vc::upcast(EcmascriptBuildNodeEntryChunk::new(
            path,
            self,
            Vc::cell(other_chunks),
            evaluatable_assets,
            module,
        ));

        Ok(asset)
    }

    #[turbo_tasks::function]
    async fn generate_chunk(
        self: Vc<Self>,
        chunk: Vc<Box<dyn Chunk>>,
    ) -> Result<Vc<Box<dyn OutputAsset>>> {
        Ok(
            if let Some(ecmascript_chunk) =
                Vc::try_resolve_downcast_type::<EcmascriptChunk>(chunk).await?
            {
                Vc::upcast(EcmascriptBuildNodeChunk::new(self, ecmascript_chunk))
            } else if let Some(output_asset) =
                Vc::try_resolve_sidecast::<Box<dyn OutputAsset>>(chunk).await?
            {
                output_asset
            } else {
                bail!("Unable to generate output asset for chunk");
            },
        )
    }
}

impl BuildChunkingContext {
    async fn get_chunk_assets(
        self: Vc<Self>,
        entry_chunk: Vc<Box<dyn Chunk>>,
        evaluatable_assets: Vc<EvaluatableAssets>,
    ) -> Result<Vec<Vc<Box<dyn OutputAsset>>>> {
        let evaluatable_assets_ref = evaluatable_assets.await?;

        let mut chunks: IndexSet<_> = evaluatable_assets_ref
            .iter()
            .map({
                move |evaluatable_asset| async move {
                    evaluatable_asset
                        .as_root_chunk(Vc::upcast(self))
                        .resolve()
                        .await
                }
            })
            .try_join()
            .await?
            .into_iter()
            .collect();

        chunks.insert(entry_chunk.resolve().await?);

        let chunks = get_parallel_chunks(chunks);

        let chunks = get_optimized_chunks(chunks.await?).await?;

        Ok(chunks
            .await?
            .iter()
            .map(|chunk| self.generate_chunk(*chunk))
            .collect())
    }
}

#[turbo_tasks::value_impl]
impl ChunkingContext for BuildChunkingContext {
    #[turbo_tasks::function]
    fn context_path(&self) -> Vc<FileSystemPath> {
        self.context_path
    }

    #[turbo_tasks::function]
    fn output_root(&self) -> Vc<FileSystemPath> {
        self.output_root
    }

    #[turbo_tasks::function]
    fn environment(&self) -> Vc<Environment> {
        self.environment
    }

    #[turbo_tasks::function]
    async fn asset_url(self: Vc<Self>, ident: Vc<AssetIdent>) -> Result<Vc<String>> {
        let this = self.await?;
        let asset_path = ident.path().await?.to_string();
        let asset_path = asset_path
            .strip_prefix(&format!("{}/", this.client_root.await?.path))
            .context("expected client root to contain asset path")?;

        Ok(Vc::cell(format!(
            "{}{}",
            this.asset_prefix
                .await?
                .as_ref()
                .map(|s| s.to_owned())
                .unwrap_or_else(|| "/".to_owned()),
            asset_path
        )))
    }

    #[turbo_tasks::function]
    async fn chunk_path(
        &self,
        ident: Vc<AssetIdent>,
        extension: String,
    ) -> Result<Vc<FileSystemPath>> {
        let root_path = self.chunk_root_path;
        let root_path = if let Some(layer) = self.layer.as_deref() {
            root_path.join(layer.to_string())
        } else {
            root_path
        };
        let name = ident.output_name(self.context_path, extension).await?;
        Ok(root_path.join(name.clone_value()))
    }

    #[turbo_tasks::function]
    fn reference_chunk_source_maps(&self, _chunk: Vc<Box<dyn OutputAsset>>) -> Vc<bool> {
        Vc::cell(true)
    }

    #[turbo_tasks::function]
    async fn can_be_in_same_chunk(
        &self,
        asset_a: Vc<Box<dyn Module>>,
        asset_b: Vc<Box<dyn Module>>,
    ) -> Result<Vc<bool>> {
        let parent_dir = asset_a.ident().path().parent().await?;

        let path = asset_b.ident().path().await?;
        if let Some(rel_path) = parent_dir.get_path_to(&path) {
            if !rel_path.starts_with("node_modules/") && !rel_path.contains("/node_modules/") {
                return Ok(Vc::cell(true));
            }
        }

        Ok(Vc::cell(false))
    }

    #[turbo_tasks::function]
    async fn asset_path(
        &self,
        content_hash: String,
        original_asset_ident: Vc<AssetIdent>,
    ) -> Result<Vc<FileSystemPath>> {
        let source_path = original_asset_ident.path().await?;
        let basename = source_path.file_name();
        let asset_path = match source_path.extension_ref() {
            Some(ext) => format!(
                "{basename}.{content_hash}.{ext}",
                basename = &basename[..basename.len() - ext.len() - 1],
                content_hash = &content_hash[..8]
            ),
            None => format!(
                "{basename}.{content_hash}",
                content_hash = &content_hash[..8]
            ),
        };
        Ok(self.asset_root_path.join(asset_path))
    }

    #[turbo_tasks::function]
    fn layer(&self) -> Vc<String> {
        Vc::cell(self.layer.clone().unwrap_or_default())
    }

    #[turbo_tasks::function]
    async fn with_layer(self: Vc<Self>, layer: String) -> Result<Vc<Self>> {
        let mut chunking_context = self.await?.clone_value();
        chunking_context.layer = (!layer.is_empty()).then(|| layer.to_string());
        Ok(BuildChunkingContext::new(Value::new(chunking_context)))
    }

    #[turbo_tasks::function]
    async fn chunk_group(
        self: Vc<Self>,
        entry_chunk: Vc<Box<dyn Chunk>>,
    ) -> Result<Vc<OutputAssets>> {
        let parallel_chunks = get_parallel_chunks([entry_chunk]).await?;

        let optimized_chunks = get_optimized_chunks(parallel_chunks).await?;

        let assets: Vec<Vc<Box<dyn OutputAsset>>> = optimized_chunks
            .await?
            .iter()
            .map(|chunk| self.generate_chunk(*chunk))
            .collect();

        Ok(Vc::cell(assets))
    }

    #[turbo_tasks::function]
    async fn evaluated_chunk_group(
        self: Vc<Self>,
        _entry_chunk: Vc<Box<dyn Chunk>>,
        _evaluatable_assets: Vc<EvaluatableAssets>,
    ) -> Result<Vc<OutputAssets>> {
        // TODO(alexkirsz) This method should be part of a separate trait that is
        // only implemented for client/edge runtimes.
        bail!("the build chunking context does not support evaluated chunk groups")
    }
}

#[turbo_tasks::value_impl]
impl EcmascriptChunkingContext for BuildChunkingContext {}

async fn get_parallel_chunks<I>(entries: I) -> Result<impl Iterator<Item = Vc<Box<dyn Chunk>>>>
where
    I: IntoIterator<Item = Vc<Box<dyn Chunk>>>,
{
    Ok(AdjacencyMap::new()
        .skip_duplicates()
        .visit(entries, |chunk: Vc<Box<dyn Chunk>>| async move {
            Ok(chunk
                .parallel_chunks()
                .await?
                .iter()
                .copied()
                .collect::<Vec<_>>()
                .into_iter())
        })
        .await
        .completed()?
        .into_inner()
        .into_reverse_topological())
}

async fn get_optimized_chunks<I>(chunks: I) -> Result<Vc<Chunks>>
where
    I: IntoIterator<Item = Vc<Box<dyn Chunk>>>,
{
    let mut ecmascript_chunks = vec![];
    let mut css_chunks = vec![];
    let mut other_chunks = vec![];

    for chunk in chunks.into_iter() {
        if let Some(ecmascript_chunk) =
            Vc::try_resolve_downcast_type::<EcmascriptChunk>(chunk).await?
        {
            ecmascript_chunks.push(ecmascript_chunk);
        } else if let Some(css_chunk) = Vc::try_resolve_downcast_type::<CssChunk>(chunk).await? {
            css_chunks.push(css_chunk);
        } else {
            other_chunks.push(chunk);
        }
    }

    // TODO(WEB-403) Optimize pass here.

    let chunks = ecmascript_chunks
        .iter()
        .copied()
        .map(Vc::upcast)
        .chain(css_chunks.iter().copied().map(Vc::upcast))
        .chain(other_chunks)
        .collect();

    Ok(Vc::cell(chunks))
}
