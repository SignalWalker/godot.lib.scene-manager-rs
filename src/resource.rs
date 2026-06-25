use std::{cell::RefCell, rc::Rc, task::Waker};

use futures::FutureExt;
use godot::{
    classes::{
        Engine, INode, Node, Object, PackedScene, Resource, ResourceLoader, SceneTree,
        class_macros::{
            private::virtuals::{
                Xrvrs::Gd,
                ZipReader::{GString, StringName, Variant},
            },
            sys::VariantType,
        },
        node::{InternalMode, ProcessMode},
        resource_loader::{CacheMode, ThreadLoadStatus},
    },
    meta::AsArg,
    obj::{Base, NewAlloc, Singleton, WithBaseField},
    register::{GodotClass, godot_api},
};

#[derive(Debug, thiserror::Error)]
pub enum ThreadedLoadError {
    #[error("could not find a resource at the given path: {0}")]
    MissingResource(GString),
    #[error("could not find scene tree")]
    NoTree,
    #[error("found scene tree, but it was of an unrecognized type")]
    UnrecognizedTree,
    #[error("load_threaded_get_status returned LOADED, but load_threaded_get returned nothing")]
    ResultMissing,
    #[error(
        "unknown error occurred during loading (ResourceLoader.load_threaded_get_status does not return error information)"
    )]
    Failed,
    #[error("the requested resource is invalid")]
    InvalidResource,
    #[error("unrecognized load status: {0:?}")]
    UnrecognizedLoadStatus(ThreadLoadStatus),
}

#[derive(Debug, thiserror::Error)]
pub enum ThreadedNodeLoadError {
    #[error(transparent)]
    Load(#[from] ThreadedLoadError),
    #[error("null argument")]
    NullArgument,
    #[error("unrecognized argument: {0}")]
    UnrecognizedArgument(Variant),
    #[error("the loaded resource was not a PackedScene: {0}")]
    NotAPackedScene(Gd<Resource>),
    #[error(transparent)]
    Object(#[from] ObjectToNodeError),
}

#[derive(thiserror::Error, Debug, Clone)]
pub enum LoadError {
    #[error(
        "unknown error occurred during loading (ResourceLoader.load does not return error information)"
    )]
    Unknown,
    #[error("null argument")]
    NullArgument,
    #[error("unrecognized argument: {0}")]
    UnrecognizedArgument(Variant),
}

#[derive(thiserror::Error, Debug, Clone)]
pub(crate) enum LoadNodeError {
    #[error(transparent)]
    Load(#[from] LoadError),
    #[error(transparent)]
    Object(#[from] ObjectToNodeError),
}

#[derive(Debug, thiserror::Error)]
pub enum LoadNodeFromPathError {
    #[error(transparent)]
    Load(#[from] LoadError),
    #[error(transparent)]
    Resource(#[from] ResourceToNodeError),
}

#[derive(Debug, thiserror::Error)]
pub enum ResourceToNodeError {
    #[error("expected PackedScene, found {0}")]
    UnrecognizedType(Gd<Resource>),
    #[error("could not instantiate Node from {0}")]
    CouldNotInstantiate(Gd<PackedScene>),
}

#[derive(thiserror::Error, Debug, Clone)]
pub enum ObjectToNodeError {
    #[error("expected Node or PackedScene, found {0}")]
    UnrecognizedType(Gd<Object>),
    #[error("could not instantiate Node from {0}")]
    CouldNotInstantiate(Gd<PackedScene>),
}

pub type LoadResult = Result<Gd<Resource>, ThreadedLoadError>;

pub fn load_threaded(
    path: GString,
    type_hint: Option<impl AsArg<GString> + Clone>,
    cache_mode: CacheMode,
    use_sub_threads: bool,
) -> Result<impl Future<Output = LoadResult>, ThreadedLoadError> {
    let mut res_loader = ResourceLoader::singleton();
    // check that the resource exists so we can return early if it doesn't
    {
        let mut exists = res_loader.exists_ex(&path);
        if let Some(type_hint) = type_hint.as_ref() {
            exists = exists.type_hint(type_hint.clone());
        }
        if !exists.done() {
            return Err(ThreadedLoadError::MissingResource(path));
        }
    }

    // TODO :: use threads instead of a weird poller node?
    let mut root = Engine::singleton()
        .get_main_loop()
        .ok_or(ThreadedLoadError::NoTree)?
        .try_cast::<SceneTree>()
        .map_err(|_| ThreadedLoadError::UnrecognizedTree)?
        .get_root();

    let future = LoadFuture::new();

    {
        let mut load_req = res_loader
            .load_threaded_request_ex(&path)
            .cache_mode(cache_mode)
            .use_sub_threads(use_sub_threads);

        if let Some(type_hint) = type_hint {
            load_req = load_req.type_hint(type_hint);
        }
        load_req.done();
    }

    let mut poller = LoadPoller::new_alloc();
    poller.bind_mut().state = PollerState::Ready {
        path: path.clone(),
        future: future.clone(),
    };

    root.run_deferred_gd(move |mut root| {
        root.add_child_ex(&poller)
            .internal(InternalMode::BACK)
            .done();
    });

    Ok(future)
}

pub fn load_threaded_to_node(
    path: GString,
    cache_mode: CacheMode,
    use_sub_threads: bool,
) -> Result<impl Future<Output = Result<Gd<Node>, ThreadedNodeLoadError>>, ThreadedNodeLoadError> {
    Ok(
        load_threaded(path, Some("PackedScene"), cache_mode, use_sub_threads)?.map(|res| {
            let scene = res?
                .try_cast::<PackedScene>()
                .map_err(ThreadedNodeLoadError::NotAPackedScene)?;
            scene
                .instantiate()
                .ok_or(ObjectToNodeError::CouldNotInstantiate(scene).into())
        }),
    )
}

pub fn load_from_path(
    path: impl AsArg<GString>,
    cache_mode: CacheMode,
    type_hint: Option<impl AsArg<GString>>,
) -> Option<Gd<Resource>> {
    let mut loader = ResourceLoader::singleton();
    let mut load = loader.load_ex(path).cache_mode(cache_mode);
    if let Some(type_hint) = type_hint {
        load = load.type_hint(type_hint);
    }
    load.done()
}

fn resource_to_node(mut res: Gd<Resource>) -> Result<Gd<Node>, ResourceToNodeError> {
    res = match res.try_cast::<PackedScene>() {
        Ok(scene) => {
            return scene
                .instantiate()
                .ok_or(ResourceToNodeError::CouldNotInstantiate(scene));
        }
        Err(obj) => obj,
    };
    Err(ResourceToNodeError::UnrecognizedType(res))
}

pub fn load_node_from_path(
    path: impl AsArg<GString>,
    cache_mode: CacheMode,
) -> Result<Gd<Node>, LoadNodeFromPathError> {
    resource_to_node(
        load_from_path(path, cache_mode, Some("PackedScene"))
            .ok_or(LoadNodeFromPathError::Load(LoadError::Unknown))?,
    )
    .map_err(From::from)
}

pub(crate) fn load_something(
    arg: Variant,
    cache_mode: CacheMode,
    type_hint: Option<impl AsArg<GString>>,
) -> Result<Gd<Object>, LoadError> {
    match arg.get_type() {
        VariantType::STRING => {
            let path = arg.try_to::<GString>().expect("should be a gstring");
            load_from_path(&path, cache_mode, type_hint)
                .ok_or(LoadError::Unknown)
                .map(Gd::upcast)
        }
        VariantType::STRING_NAME => {
            let path = arg.try_to::<StringName>().expect("should be a stringname");
            load_from_path(&GString::from(&path), cache_mode, type_hint)
                .ok_or(LoadError::Unknown)
                .map(Gd::upcast)
        }
        VariantType::OBJECT => Ok(arg.try_to::<Gd<Object>>().expect("should be an object")),
        VariantType::NIL => Err(LoadError::NullArgument),
        _ => Err(LoadError::UnrecognizedArgument(arg.clone())),
    }
}

fn object_to_node(mut obj: Gd<Object>) -> Result<Gd<Node>, ObjectToNodeError> {
    obj = match obj.try_cast::<Node>() {
        Ok(node) => return Ok(node),
        Err(obj) => obj,
    };
    obj = match obj.try_cast::<PackedScene>() {
        Ok(scene) => {
            return scene
                .instantiate()
                .ok_or(ObjectToNodeError::CouldNotInstantiate(scene));
        }
        Err(obj) => obj,
    };
    Err(ObjectToNodeError::UnrecognizedType(obj))
}

pub(crate) fn load_something_to_node(
    arg: Variant,
    cache_mode: CacheMode,
) -> Result<Gd<Node>, LoadNodeError> {
    object_to_node(load_something(arg, cache_mode, Some("PackedScene"))?)
        .map_err(LoadNodeError::from)
}

pub(crate) fn load_threaded_something_to_node(
    arg: Variant,
    cache_mode: CacheMode,
    use_sub_threads: bool,
) -> Result<impl Future<Output = Result<Gd<Node>, ThreadedNodeLoadError>>, ThreadedNodeLoadError> {
    match arg.get_type() {
        VariantType::STRING => {
            let path = arg.try_to::<GString>().expect("should be a gstring");
            load_threaded_to_node(path, cache_mode, use_sub_threads).map(FutureExt::boxed_local)
        }
        VariantType::STRING_NAME => {
            let path = arg.try_to::<StringName>().expect("should be a stringname");
            load_threaded_to_node(GString::from(&path), cache_mode, use_sub_threads)
                .map(FutureExt::boxed_local)
        }
        VariantType::OBJECT => {
            let obj = arg.try_to::<Gd<Object>>().expect("should be an object");
            object_to_node(obj)
                .map_err(ThreadedNodeLoadError::from)
                .map(|res| std::future::ready(Ok(res)).boxed_local())
        }
        VariantType::NIL => Err(ThreadedNodeLoadError::NullArgument),
        _ => Err(ThreadedNodeLoadError::UnrecognizedArgument(arg.clone())),
    }
}

#[derive(Clone)]
struct LoadFuture {
    result: Rc<RefCell<Option<LoadResult>>>,
    waker: Rc<RefCell<Option<Waker>>>,
}

impl LoadFuture {
    fn new() -> Self {
        Self {
            result: Default::default(),
            waker: Default::default(),
        }
    }

    fn finish(&self, result: Result<Gd<Resource>, ThreadedLoadError>) {
        *self.result.borrow_mut() = Some(result);
        if let Some(waker) = self.waker.borrow_mut().take() {
            waker.wake();
        }
    }
}

impl std::future::Future for LoadFuture {
    type Output = LoadResult;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let res = self.result.borrow_mut().take();
        if let Some(res) = res {
            std::task::Poll::Ready(res)
        } else {
            *self.waker.borrow_mut() = Some(cx.waker().clone());
            std::task::Poll::Pending
        }
    }
}

enum PollerState {
    Initializing,
    Ready { path: GString, future: LoadFuture },
}

#[derive(GodotClass)]
#[class(base = Node, internal)]
struct LoadPoller {
    base: Base<Node>,

    state: PollerState,
}

#[godot_api]
impl INode for LoadPoller {
    fn init(base: Base<Node>) -> Self {
        Self {
            base,
            state: PollerState::Initializing,
        }
    }

    fn enter_tree(&mut self) {
        self.base_mut().set_process_mode(ProcessMode::ALWAYS);
    }

    fn process(&mut self, _delta: f32) {
        let PollerState::Ready { path, future } = &self.state else {
            tracing::error!("LoadPoller not initialized");
            return;
        };
        let mut res_loader = ResourceLoader::singleton();
        match res_loader.load_threaded_get_status(path) {
            ThreadLoadStatus::IN_PROGRESS => {
                // nothing happens here
            }
            ThreadLoadStatus::LOADED => {
                future.finish(
                    res_loader
                        .load_threaded_get(path)
                        .ok_or(ThreadedLoadError::ResultMissing),
                );
                self.base_mut().queue_free();
            }
            ThreadLoadStatus::FAILED => {
                future.finish(Err(ThreadedLoadError::Failed));
                self.base_mut().queue_free();
            }
            ThreadLoadStatus::INVALID_RESOURCE => {
                future.finish(Err(ThreadedLoadError::InvalidResource));
                self.base_mut().queue_free();
            }
            status => {
                future.finish(Err(ThreadedLoadError::UnrecognizedLoadStatus(status)));
                self.base_mut().queue_free();
            }
        }
    }
}
