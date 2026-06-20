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
pub enum LoadError {
    #[error("could not find scene tree")]
    NoTree,
    #[error("found scene tree, but it was of an unrecognized type")]
    UnrecognizedTree,
    #[error("scene tree has no root node")]
    NoRoot,
    #[error("load_threaded_get_status returned LOADED, but load_threaded_get returned nothing")]
    ResultMissing,
    #[error("unknown error occurred during loading")]
    Failed,
    #[error("the requested resource is invalid")]
    InvalidResource,
    #[error("unrecognized load status: {0:?}")]
    UnrecognizedLoadStatus(ThreadLoadStatus),
}

#[derive(Debug, thiserror::Error)]
pub enum NodeLoadError {
    #[error(transparent)]
    Load(#[from] LoadError),
    #[error("null argument")]
    NullArgument,
    #[error("unrecognized argument: {0}")]
    UnrecognizedArgument(Variant),
    #[error("the loaded resource was not a PackedScene: {0}")]
    NotAPackedScene(Gd<Resource>),
    #[error("could not instantiate the loaded scene: {0}")]
    CouldNotInstantiate(Gd<PackedScene>),
    #[error("unrecognized argument: {0}")]
    UnrecognizedObject(Gd<Object>),
}

pub type LoadResult = Result<Gd<Resource>, LoadError>;

pub fn load_threaded(
    path: GString,
    type_hint: Option<impl AsArg<GString>>,
    cache_mode: CacheMode,
    use_sub_threads: bool,
) -> Result<impl Future<Output = LoadResult>, LoadError> {
    // TODO :: use threads instead of a weird poller node?
    let mut root = Engine::singleton()
        .get_main_loop()
        .ok_or(LoadError::NoTree)?
        .try_cast::<SceneTree>()
        .map_err(|_| LoadError::UnrecognizedTree)?
        .get_root()
        .ok_or(LoadError::NoRoot)?;

    let future = LoadFuture::new();

    {
        let mut res_loader = ResourceLoader::singleton();
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
) -> Result<impl Future<Output = Result<Gd<Node>, NodeLoadError>>, NodeLoadError> {
    Ok(
        load_threaded(path, Some("PackedScene"), cache_mode, use_sub_threads)?.map(|res| {
            let scene = res?
                .try_cast::<PackedScene>()
                .map_err(NodeLoadError::NotAPackedScene)?;
            scene
                .instantiate()
                .ok_or(NodeLoadError::CouldNotInstantiate(scene))
        }),
    )
}

pub(crate) fn load_something_to_node(
    arg: Variant,
    cache_mode: CacheMode,
    use_sub_threads: bool,
) -> Result<impl Future<Output = Result<Gd<Node>, NodeLoadError>>, NodeLoadError> {
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
            let mut obj = arg.try_to::<Gd<Object>>().expect("should be an object");
            obj = match obj.try_cast::<Node>() {
                Ok(node) => return Ok(std::future::ready(Ok(node)).boxed_local()),
                Err(obj) => obj,
            };
            obj = match obj.try_cast::<PackedScene>() {
                Ok(scene) => {
                    return scene
                        .instantiate()
                        .ok_or(NodeLoadError::CouldNotInstantiate(scene))
                        .map(|res| std::future::ready(Ok(res)).boxed_local());
                }
                Err(obj) => obj,
            };
            Err(NodeLoadError::UnrecognizedObject(obj))
        }
        VariantType::NIL => Err(NodeLoadError::NullArgument),
        _ => Err(NodeLoadError::UnrecognizedArgument(arg.clone())),
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

    fn finish(&self, result: Result<Gd<Resource>, LoadError>) {
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
                        .ok_or(LoadError::ResultMissing),
                );
                self.base_mut().queue_free();
            }
            ThreadLoadStatus::FAILED => {
                future.finish(Err(LoadError::Failed));
                self.base_mut().queue_free();
            }
            ThreadLoadStatus::INVALID_RESOURCE => {
                future.finish(Err(LoadError::InvalidResource));
                self.base_mut().queue_free();
            }
            status => {
                future.finish(Err(LoadError::UnrecognizedLoadStatus(status)));
                self.base_mut().queue_free();
            }
        }
    }
}
