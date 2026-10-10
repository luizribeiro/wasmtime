use std::any::{Any, TypeId};
use std::mem;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::oneshot;
use wasmtime::component::{
    Component, Destination, FutureAny, FutureConsumer, FutureReader, Instance, Lift, Linker,
    Resource, ResourceType, Source, StreamAny, StreamConsumer, StreamProducer, StreamReader,
    StreamResult, Type, TypedFunc, Val, VecBuffer, types::StreamType,
};
use wasmtime::{Config, Engine, Result, Store, StoreContextMut};

#[test]
fn simple_type_conversions() -> Result<()> {
    let mut config = Config::new();
    config.wasm_component_model_async(true);
    let engine = Engine::new(&config)?;
    let mut store = Store::new(&engine, ());

    let f = FutureReader::new(&mut store, async { wasmtime::error::Ok(10_u32) })?;
    let f = f.try_into_future_any(&mut store).unwrap();
    assert!(f.clone().try_into_future_reader::<()>().is_err());
    assert!(f.clone().try_into_future_reader::<u64>().is_err());
    let f = f.try_into_future_reader::<u32>().unwrap();
    f.try_into_future_any(&mut store)
        .unwrap()
        .close(&mut store)?;

    let s = StreamReader::new(&mut store, vec![10_u32])?;
    let s = s.try_into_stream_any(&mut store).unwrap();
    assert!(s.clone().try_into_stream_reader::<()>().is_err());
    assert!(s.clone().try_into_stream_reader::<u64>().is_err());
    let s = s.try_into_stream_reader::<u32>().unwrap();
    s.try_into_stream_any(&mut store)
        .unwrap()
        .close(&mut store)?;

    Ok(())
}

#[test]
#[cfg_attr(miri, ignore)]
fn simple_type_assertions() -> Result<()> {
    let mut config = Config::new();
    config.wasm_component_model_async(true);
    let engine = Engine::new(&config)?;
    let mut store = Store::new(&engine, ());

    let component = Component::new(
        &engine,
        r#"
        (component
            (type $f (future u32))
            (type $s (stream u32))
            (core func $mk-f (canon future.new $f))
            (core func $mk-s (canon stream.new $s))

            (core module $m
                (import "" "mk-f" (func $mk-f (result i64)))
                (import "" "mk-s" (func $mk-s (result i64)))

                (func (export "x") (param i32) (result i32) local.get 0)

                (func (export "mk-f") (result i32)
                    (i32.wrap_i64 (call $mk-f)))
                (func (export "mk-s") (result i32)
                    (i32.wrap_i64 (call $mk-s)))
            )
            (core instance $i (instantiate $m
                (with "" (instance
                    (export "mk-f" (func $mk-f))
                    (export "mk-s" (func $mk-s))
                ))
            ))
            (func (export "f") (param "f" $f) (result $f)
                (canon lift (core func $i "x")))
            (func (export "s") (param "s" $s) (result $s)
                (canon lift (core func $i "x")))
            (func (export "mk-f") (result $f)
                (canon lift (core func $i "mk-f")))
            (func (export "mk-s") (result $s)
                (canon lift (core func $i "mk-s")))
        )
        "#,
    )?;

    let instance = Linker::new(&engine).instantiate(&mut store, &component)?;

    let f_t_t =
        instance.get_typed_func::<(FutureReader<u32>,), (FutureReader<u32>,)>(&mut store, "f")?;
    let f_t_a = instance.get_typed_func::<(FutureReader<u32>,), (FutureAny,)>(&mut store, "f")?;
    let f_a_t = instance.get_typed_func::<(FutureAny,), (FutureReader<u32>,)>(&mut store, "f")?;
    let f_a_a = instance.get_typed_func::<(FutureAny,), (FutureAny,)>(&mut store, "f")?;

    let s_t_t =
        instance.get_typed_func::<(StreamReader<u32>,), (StreamReader<u32>,)>(&mut store, "s")?;
    let s_t_a = instance.get_typed_func::<(StreamReader<u32>,), (StreamAny,)>(&mut store, "s")?;
    let s_a_t = instance.get_typed_func::<(StreamAny,), (StreamReader<u32>,)>(&mut store, "s")?;
    let s_a_a = instance.get_typed_func::<(StreamAny,), (StreamAny,)>(&mut store, "s")?;

    let mk_f_t = instance.get_typed_func::<(), (FutureReader<u32>,)>(&mut store, "mk-f")?;
    let mk_f_a = instance.get_typed_func::<(), (FutureAny,)>(&mut store, "mk-f")?;
    let mk_s_t = instance.get_typed_func::<(), (StreamReader<u32>,)>(&mut store, "mk-s")?;
    let mk_s_a = instance.get_typed_func::<(), (StreamAny,)>(&mut store, "mk-s")?;

    assert!(instance.get_typed_func::<(), ()>(&mut store, "f").is_err());
    assert!(
        instance
            .get_typed_func::<(u32,), (FutureReader<u32>,)>(&mut store, "f")
            .is_err()
    );
    assert!(
        instance
            .get_typed_func::<(FutureReader<u32>,), (u32,)>(&mut store, "f")
            .is_err()
    );
    assert!(
        instance
            .get_typed_func::<(FutureReader<()>,), (FutureReader<u32>,)>(&mut store, "f")
            .is_err()
    );
    assert!(
        instance
            .get_typed_func::<(FutureReader<u64>,), (FutureReader<u32>,)>(&mut store, "f")
            .is_err()
    );

    assert!(instance.get_typed_func::<(), ()>(&mut store, "s").is_err());
    assert!(
        instance
            .get_typed_func::<(u32,), (StreamReader<u32>,)>(&mut store, "s")
            .is_err()
    );
    assert!(
        instance
            .get_typed_func::<(StreamReader<u32>,), (u32,)>(&mut store, "s")
            .is_err()
    );
    assert!(
        instance
            .get_typed_func::<(StreamReader<()>,), (StreamReader<u32>,)>(&mut store, "s")
            .is_err()
    );
    assert!(
        instance
            .get_typed_func::<(StreamReader<u64>,), (StreamReader<u32>,)>(&mut store, "s")
            .is_err()
    );

    let roundtrip = |store: &mut Store<()>, f: FutureReader<u32>| -> Result<()> {
        let (f,) = f_t_t.call(&mut *store, (f,))?;
        let (f,) = f_t_a.call(&mut *store, (f,))?;
        let (f,) = f_a_a.call(&mut *store, (f,))?;
        let (mut f,) = f_a_t.call(&mut *store, (f,))?;
        f.close(&mut *store)?;
        Ok(())
    };

    let f = FutureReader::new(&mut store, async { wasmtime::error::Ok(10_u32) })?;
    roundtrip(&mut store, f)?;

    let (f,) = mk_f_t.call(&mut store, ())?;
    roundtrip(&mut store, f)?;

    let (f,) = mk_f_a.call(&mut store, ())?;
    let f = f.try_into_future_reader::<u32>()?;
    roundtrip(&mut store, f)?;

    let roundtrip = |store: &mut Store<()>, s: StreamReader<u32>| -> Result<()> {
        let (s,) = s_t_t.call(&mut *store, (s,))?;
        let (s,) = s_t_a.call(&mut *store, (s,))?;
        let (s,) = s_a_a.call(&mut *store, (s,))?;
        let (mut s,) = s_a_t.call(&mut *store, (s,))?;
        s.close(&mut *store)?;
        Ok(())
    };

    let s = StreamReader::new(&mut store, vec![10_u32])?;
    roundtrip(&mut store, s)?;

    let (s,) = mk_s_t.call(&mut store, ())?;
    roundtrip(&mut store, s)?;

    let (s,) = mk_s_a.call(&mut store, ())?;
    let s = s.try_into_stream_reader::<u32>()?;
    roundtrip(&mut store, s)?;

    Ok(())
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn stream_any_smoke() -> Result<()> {
    let mut config = Config::new();
    config.wasm_component_model_async(true);
    let engine = Engine::new(&config)?;
    let mut store = Store::new(&engine, ());
    let component = Component::new(
        &engine,
        r#"
(component
    (type $s (stream u8))

    (core module $libc (memory (export "mem") 1))
    (core instance $libc (instantiate $libc))

    (core module $m
        (import "" "stream.new" (func $stream.new (result i64)))
        (import "" "task.return" (func $task.return))
        (import "" "waitable-set.new" (func $waitable-set.new (result i32)))
        (import "" "waitable.join" (func $waitable.join (param i32 i32)))
        (import "" "waitable-set.wait" (func $waitable-set.wait (param i32 i32) (result i32)))
        (import "" "waitable-set.drop" (func $waitable-set.drop (param i32)))
        (import "" "mem" (memory 1))

        (global $w (mut i32) (i32.const 0))

        (func (export "mk") (result i32)
            (local $r i32) (local $tmp i64)
            (local.set $tmp (call $stream.new))
            (local.set $r (i32.wrap_i64 (local.get $tmp)))
            (global.set $w (i32.wrap_i64 (i64.shr_u (local.get $tmp) (i64.const 32))))
            local.get $r
        )

        (func (export "run") (result i32)
            (local $ws i32)
            (local.set $ws (call $waitable-set.new))
            (call $waitable.join (global.get $w) (local.get $ws))
            (call $waitable-set.wait (local.get $ws) (i32.const 0))
            i32.const 3 ;; EVENT_STREAM_WRITE
            i32.ne
            if unreachable end

            (if (i32.ne (i32.load (i32.const 0)) (global.get $w))
              (then unreachable))
            (if (i32.ne (i32.load (i32.const 4)) (i32.const 1)) ;; DROPPED | (0 << 4)
              (then unreachable))

            call $task.return

            i32.const 0 ;; CALLBACK_CODE_EXIT
        )

        (func (export "cb") (param i32 i32 i32) (result i32) unreachable)
    )
    (core func $stream.new (canon stream.new $s))
    (core func $task.return (canon task.return))
    (core func $waitable-set.new (canon waitable-set.new))
    (core func $waitable.join (canon waitable.join))
    (core func $waitable-set.wait (canon waitable-set.wait (memory (core memory $libc "mem"))))
    (core func $waitable-set.drop (canon waitable-set.drop))
    (core instance $i (instantiate $m
        (with "" (instance
            (export "stream.new" (func $stream.new))
            (export "task.return" (func $task.return))
            (export "waitable-set.new" (func $waitable-set.new))
            (export "waitable.join" (func $waitable.join))
            (export "waitable-set.wait" (func $waitable-set.wait))
            (export "waitable-set.drop" (func $waitable-set.drop))
            (export "mem" (memory $libc "mem"))
        ))
    ))
    (func (export "mk") (result (stream u8))
        (canon lift (core func $i "mk")))
    (func (export "run") async
        (canon lift (core func $i "run") async (callback (core func $i "cb"))))
)
        "#,
    )?;
    let instance = Linker::new(&engine).instantiate(&mut store, &component)?;
    let mk = instance.get_typed_func::<(), (StreamAny,)>(&mut store, "mk")?;
    let run = instance.get_typed_func::<(), ()>(&mut store, "run")?;
    store
        .run_concurrent(async |store| {
            let (mut stream,) = mk.call_concurrent(store, ()).await?;
            tokio::try_join! {
                async {
                    run.call_concurrent(store, ()).await?;
                    wasmtime::error::Ok(())
                },
                async {
                    store.with(|store| stream.close(store))?;
                    wasmtime::error::Ok(())
                }
            }?;
            wasmtime::error::Ok(())
        })
        .await??;
    Ok(())
}

const TAKE_FUTURES_AND_STREAMS: &str = r#"
(component
    (type $f (future u32))
    (type $s (stream u32))
    (core module $m
        (func (export "take1") (param i32))
        (func (export "take2") (param i32 i32)))
    (core instance $i (instantiate $m))
    (func (export "take1-f") (param "a" $f)
        (canon lift (core func $i "take1")))
    (func (export "take2-f") (param "a" $f) (param "b" $f)
        (canon lift (core func $i "take2")))
    (func (export "take1-s") (param "a" $s)
        (canon lift (core func $i "take1")))
    (func (export "take2-s") (param "a" $s) (param "b" $s)
        (canon lift (core func $i "take2")))
)
"#;

#[test]
#[cfg_attr(miri, ignore)]
fn future_and_stream_lowered_at_most_once() -> Result<()> {
    let mut config = Config::new();
    config.wasm_component_model_async(true);
    let engine = Engine::new(&config)?;
    let component = Component::new(&engine, TAKE_FUTURES_AND_STREAMS)?;
    let linker = Linker::new(&engine);
    let new_store = || Store::new(&engine, ());

    // Two clones of one `FutureAny` passed to the same call.
    {
        let mut store = new_store();
        let instance = linker.instantiate(&mut store, &component)?;
        let take2 = instance.get_typed_func::<(FutureAny, FutureAny), ()>(&mut store, "take2-f")?;
        let f = FutureReader::new(&mut store, async { wasmtime::error::Ok(7_u32) })?
            .try_into_future_any(&mut store)?;
        let mut g = f.clone();
        assert!(take2.call(&mut store, (f, g.clone())).is_err());
        assert!(g.close(&mut store).is_err());
    }

    // Two clones of one `StreamAny` passed to the same call.
    {
        let mut store = new_store();
        let instance = linker.instantiate(&mut store, &component)?;
        let take2 = instance.get_typed_func::<(StreamAny, StreamAny), ()>(&mut store, "take2-s")?;
        let s = StreamReader::new(&mut store, vec![7_u32])?.try_into_stream_any(&mut store)?;
        let mut t = s.clone();
        assert!(take2.call(&mut store, (s, t.clone())).is_err());
        assert!(t.close(&mut store).is_err());
    }

    // The same `Val::Future` lowered in two calls.
    {
        let mut store = new_store();
        let instance = linker.instantiate(&mut store, &component)?;
        let take1 = instance.get_func(&mut store, "take1-f").unwrap();
        let f = FutureReader::new(&mut store, async { wasmtime::error::Ok(7_u32) })?
            .try_into_future_any(&mut store)?;
        let f = Val::Future(f);
        take1.call(&mut store, &[f.clone()], &mut [])?;
        assert!(take1.call(&mut store, &[f.clone()], &mut []).is_err());
        let Val::Future(mut f) = f else {
            unreachable!()
        };
        assert!(f.close(&mut store).is_err());
    }

    // The same `Val::Stream` lowered in two calls.
    {
        let mut store = new_store();
        let instance = linker.instantiate(&mut store, &component)?;
        let take1 = instance.get_func(&mut store, "take1-s").unwrap();
        let s = StreamReader::new(&mut store, vec![7_u32])?.try_into_stream_any(&mut store)?;
        let s = Val::Stream(s);
        take1.call(&mut store, &[s.clone()], &mut [])?;
        assert!(take1.call(&mut store, &[s.clone()], &mut []).is_err());
        let Val::Stream(mut s) = s else {
            unreachable!()
        };
        assert!(s.close(&mut store).is_err());
    }

    // A `FutureReader` lowered twice by reference.
    {
        let mut store = new_store();
        let instance = linker.instantiate(&mut store, &component)?;
        let take1 = instance.get_typed_func::<(&FutureReader<u32>,), ()>(&mut store, "take1-f")?;
        let mut f = FutureReader::new(&mut store, async { wasmtime::error::Ok(7_u32) })?;
        take1.call(&mut store, (&f,))?;
        assert!(take1.call(&mut store, (&f,)).is_err());
        assert!(f.close(&mut store).is_err());
    }

    // A `StreamReader` lowered twice by reference.
    {
        let mut store = new_store();
        let instance = linker.instantiate(&mut store, &component)?;
        let take1 = instance.get_typed_func::<(&StreamReader<u32>,), ()>(&mut store, "take1-s")?;
        let mut s = StreamReader::new(&mut store, vec![7_u32])?;
        take1.call(&mut store, (&s,))?;
        assert!(take1.call(&mut store, (&s,)).is_err());
        assert!(s.close(&mut store).is_err());
    }

    // Closing clones more than once.
    {
        let mut store = new_store();
        let mut f = FutureReader::new(&mut store, async { wasmtime::error::Ok(7_u32) })?
            .try_into_future_any(&mut store)?;
        let mut g = f.clone();
        f.close(&mut store)?;
        assert!(g.close(&mut store).is_err());
        assert!(f.close(&mut store).is_err());
    }

    // A stale clone must not alias a new future which reuses the old one's
    // table slot.
    {
        let mut store = new_store();
        let instance = linker.instantiate(&mut store, &component)?;
        let take1 = instance.get_typed_func::<(&FutureAny,), ()>(&mut store, "take1-f")?;
        let f = FutureReader::new(&mut store, async { wasmtime::error::Ok(7_u32) })?
            .try_into_future_any(&mut store)?;
        f.clone().close(&mut store)?;
        let mut g = FutureReader::new(&mut store, async { wasmtime::error::Ok(8_u32) })?
            .try_into_future_any(&mut store)?;
        assert!(take1.call(&mut store, (&f,)).is_err());
        g.close(&mut store)?;
    }

    Ok(())
}

const ECHO_STRING_FUTURES_AND_STREAMS: &str = r#"
(component
    (type $f (future string))
    (type $s (stream string))
    (core module $m
        (func (export "echo") (param i32) (result i32) local.get 0))
    (core instance $i (instantiate $m))
    (func (export "echo-f") (param "f" $f) (result $f)
        (canon lift (core func $i "echo")))
    (func (export "echo-s") (param "s" $s) (result $s)
        (canon lift (core func $i "echo")))
)
"#;

/// A consumer which takes everything it's given, using `read`, and sends it
/// over `done` when dropped.
struct Collect<T> {
    read: ReadFn<T>,
    taken: Vec<T>,
    done: Option<oneshot::Sender<Vec<T>>>,
}

type ReadFn<T> = fn(&mut Source<'_, T>, StoreContextMut<'_, ()>, &mut Vec<T>) -> Result<()>;

fn read_typed<T: Lift + 'static>(
    src: &mut Source<'_, T>,
    store: StoreContextMut<'_, ()>,
    buf: &mut Vec<T>,
) -> Result<()> {
    src.read(store, buf)
}

fn read_vals(
    src: &mut Source<'_, Val>,
    store: StoreContextMut<'_, ()>,
    buf: &mut Vec<Val>,
) -> Result<()> {
    src.read_vals(store, buf)
}

impl<T> Collect<T> {
    fn with_read(read: ReadFn<T>) -> (Self, oneshot::Receiver<Vec<T>>) {
        let (tx, rx) = oneshot::channel();
        let consumer = Collect {
            read,
            taken: Vec::new(),
            done: Some(tx),
        };
        (consumer, rx)
    }

    fn new() -> (Self, oneshot::Receiver<Vec<T>>)
    where
        T: Lift + 'static,
    {
        Self::with_read(read_typed::<T>)
    }

    /// Reads everything `src` has, keeping what was read even if reading
    /// fails partway through.
    fn take(&mut self, mut store: StoreContextMut<()>, mut src: Source<'_, T>) -> Result<()>
    where
        T: 'static,
    {
        let mut buf = Vec::with_capacity(src.remaining(&mut store));
        let result = (self.read)(&mut src, store, &mut buf);
        self.taken.extend(buf);
        result
    }
}

impl Collect<Val> {
    fn vals() -> (Self, oneshot::Receiver<Vec<Val>>) {
        Self::with_read(read_vals)
    }
}

impl<T> Drop for Collect<T> {
    fn drop(&mut self) {
        let _ = self.done.take().unwrap().send(mem::take(&mut self.taken));
    }
}

impl<T: Send + Unpin + 'static> StreamConsumer<()> for Collect<T> {
    type Item = T;

    fn poll_consume(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        store: StoreContextMut<()>,
        src: Source<'_, T>,
        _finish: bool,
    ) -> Poll<Result<StreamResult>> {
        self.take(store, src)?;
        Poll::Ready(Ok(StreamResult::Completed))
    }
}

impl<T: Send + Unpin + 'static> FutureConsumer<()> for Collect<T> {
    type Item = T;

    fn poll_consume(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        store: StoreContextMut<()>,
        src: Source<'_, T>,
        _finish: bool,
    ) -> Poll<Result<()>> {
        self.take(store, src)?;
        Poll::Ready(Ok(()))
    }
}

async fn instantiate_echo() -> Result<(Store<()>, Instance)> {
    let mut config = Config::new();
    config.wasm_component_model_async(true);
    let engine = Engine::new(&config)?;
    let component = Component::new(&engine, ECHO_STRING_FUTURES_AND_STREAMS)?;
    let mut store = Store::new(&engine, ());
    let instance = Linker::new(&engine)
        .instantiate_async(&mut store, &component)
        .await?;
    Ok((store, instance))
}

/// Sends a host-produced `stream<string>` of `items` through a guest and back
/// as a `StreamReader<T>`, then pipes it into a host consumer of `T`,
/// returning what that consumed.
async fn echo_host_stream<T: Lift + Send + Unpin + 'static>(items: Vec<String>) -> Result<Vec<T>> {
    let (mut store, instance) = instantiate_echo().await?;
    let echo = instance
        .get_typed_func::<(StreamReader<String>,), (StreamReader<T>,)>(&mut store, "echo-s")?;
    let reader = StreamReader::new(&mut store, items)?;
    let (reader,) = echo.call_async(&mut store, (reader,)).await?;
    let (consumer, done) = Collect::new();
    reader.pipe(&mut store, consumer)?;
    Ok(store.run_concurrent(async |_| done.await).await??)
}

/// Like `echo_host_stream`, but for a `future<string>` resolving to `item`.
async fn echo_host_future<T: Lift + Send + Unpin + 'static>(item: String) -> Result<Vec<T>> {
    let (mut store, instance) = instantiate_echo().await?;
    let echo = instance
        .get_typed_func::<(FutureReader<String>,), (FutureReader<T>,)>(&mut store, "echo-f")?;
    let reader = FutureReader::new(&mut store, async { wasmtime::error::Ok(item) })?;
    let (reader,) = echo.call_async(&mut store, (reader,)).await?;
    let (consumer, done) = Collect::new();
    reader.pipe(&mut store, consumer)?;
    Ok(store.run_concurrent(async |_| done.await).await??)
}

fn assert_type_mismatch(err: &wasmtime::Error) {
    let msg = format!("{err:?}");
    assert!(
        msg.contains("String` items cannot be read by a host consumer of `")
            && msg.contains("Box<str>` items"),
        "unexpected error: {msg}"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn host_stream_read_as_same_rust_type() -> Result<()> {
    let items = vec!["a".to_string(), "b".to_string()];
    assert_eq!(echo_host_stream::<String>(items.clone()).await?, items);
    Ok(())
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn host_stream_read_as_other_rust_type() -> Result<()> {
    let items = vec!["a".to_string(), "b".to_string()];
    let err = echo_host_stream::<Box<str>>(items).await.unwrap_err();
    assert_type_mismatch(&err);
    Ok(())
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn empty_host_stream_read_as_other_rust_type() -> Result<()> {
    assert!(echo_host_stream::<Box<str>>(Vec::new()).await?.is_empty());
    Ok(())
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn host_future_read_as_same_rust_type() -> Result<()> {
    assert_eq!(echo_host_future::<String>("a".to_string()).await?, ["a"]);
    Ok(())
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn host_future_read_as_other_rust_type() -> Result<()> {
    let err = echo_host_future::<Box<str>>("a".to_string())
        .await
        .unwrap_err();
    assert_type_mismatch(&err);
    Ok(())
}

/// Returns a component whose `new` export creates a stream of type `$s`,
/// defined by `types` (e.g. `(type $s (stream u32))`), whose `write` export
/// writes the first `n` items encoded at address 100 by `data`, and whose
/// `close` export drops the writable end.
fn stream_writer(types: &str, data: &str) -> String {
    format!(
        r#"
(component
  {types}
  (core module $libc (memory (export "mem") 1))
  (core instance $libc (instantiate $libc))
  (core func $stream.new (canon stream.new $s))
  (core func $stream.write
    (canon stream.write $s async (memory (core memory $libc "mem"))))
  (core func $stream.drop-writable (canon stream.drop-writable $s))
  (core module $m
    (import "" "mem" (memory 1))
    (import "" "stream.new" (func $stream.new (result i64)))
    (import "" "stream.write"
      (func $stream.write (param i32 i32 i32) (result i32)))
    (import "" "stream.drop-writable" (func $stream.drop-writable (param i32)))

    (global $w (mut i32) (i32.const 0))
    {data}

    (func (export "new") (result i32)
      (local $rw i64)
      (local.set $rw (call $stream.new))
      (global.set $w (i32.wrap_i64 (i64.shr_u (local.get $rw) (i64.const 32))))
      (i32.wrap_i64 (local.get $rw)))
    (func (export "write") (param $n i32) (result i32)
      (call $stream.write (global.get $w) (i32.const 100) (local.get $n)))
    (func (export "close")
      (call $stream.drop-writable (global.get $w))))
  (core instance $i (instantiate $m (with "" (instance
    (export "mem" (memory $libc "mem"))
    (export "stream.new" (func $stream.new))
    (export "stream.write" (func $stream.write))
    (export "stream.drop-writable" (func $stream.drop-writable))))))
  (func (export "new") (result $s) (canon lift (core func $i "new")))
  (func (export "write") (param "n" u32) (result u32)
    (canon lift (core func $i "write")))
  (func (export "close") (canon lift (core func $i "close")))
)
"#
    )
}

struct StreamWriter {
    store: Store<()>,
    stream: StreamAny,
    write: TypedFunc<(u32,), (u32,)>,
    close: TypedFunc<(), ()>,
}

impl StreamWriter {
    async fn new(types: &str, data: &str) -> Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model_async(true);
        let engine = Engine::new(&config)?;
        let component = Component::new(&engine, stream_writer(types, data))?;
        let mut store = Store::new(&engine, ());
        let instance = Linker::new(&engine)
            .instantiate_async(&mut store, &component)
            .await?;
        let new = instance.get_typed_func::<(), (StreamAny,)>(&mut store, "new")?;
        let write = instance.get_typed_func::<(u32,), (u32,)>(&mut store, "write")?;
        let close = instance.get_typed_func::<(), ()>(&mut store, "close")?;
        let (stream,) = new.call_async(&mut store, ()).await?;
        Ok(StreamWriter {
            store,
            stream,
            write,
            close,
        })
    }

    /// Pipes the stream into a `Val` consumer, has the guest write `n` items
    /// and close the stream, and returns what the consumer took.
    async fn read_vals(mut self, n: u32) -> Result<Vec<Val>> {
        let (consumer, done) = Collect::vals();
        self.stream.pipe(&mut self.store, consumer)?;
        let (code,) = self.write.call_async(&mut self.store, (n,)).await?;
        assert_eq!(code, n << 4); // COMPLETED(n)
        self.close.call_async(&mut self.store, ()).await?;
        Ok(self.store.run_concurrent(async |_| done.await).await??)
    }
}

fn record(fields: &[(&str, Val)]) -> Val {
    Val::Record(
        fields
            .iter()
            .map(|(name, val)| (name.to_string(), val.clone()))
            .collect(),
    )
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn guest_stream_read_as_vals() -> Result<()> {
    // Strings and lists below point at "abc" stored at address 200.
    let abc = r#"(data (i32.const 200) "abc")"#;
    let cases = [
        (
            "(type $s (stream u32))".to_string(),
            r#"(data (i32.const 100) "\01\00\00\00\02\00\00\00")"#.to_string(),
            vec![Val::U32(1), Val::U32(2)],
        ),
        (
            "(type $s (stream string))".to_string(),
            format!(
                r#"(data (i32.const 100) "\c8\00\00\00\02\00\00\00\ca\00\00\00\01\00\00\00") {abc}"#
            ),
            vec![Val::String("ab".into()), Val::String("c".into())],
        ),
        (
            r#"(type $r (record (field "a" u32) (field "b" string)))
               (export $r' "r" (type $r))
               (type $s (stream $r'))"#
                .to_string(),
            format!(r#"(data (i32.const 100) "\07\00\00\00\c8\00\00\00\03\00\00\00") {abc}"#),
            vec![record(&[
                ("a", Val::U32(7)),
                ("b", Val::String("abc".into())),
            ])],
        ),
        (
            "(type $s (stream (option u32)))".to_string(),
            r#"(data (i32.const 100) "\01\00\00\00\05\00\00\00\00\00\00\00\00\00\00\00")"#
                .to_string(),
            vec![Val::Option(Some(Box::new(Val::U32(5)))), Val::Option(None)],
        ),
        (
            "(type $s (stream (list u8)))".to_string(),
            format!(r#"(data (i32.const 100) "\c8\00\00\00\03\00\00\00") {abc}"#),
            vec![Val::List(vec![Val::U8(b'a'), Val::U8(b'b'), Val::U8(b'c')])],
        ),
    ];
    for (types, data, expected) in cases {
        let writer = StreamWriter::new(&types, &data).await?;
        assert!(writer.stream.ty().unwrap().ty().is_some());
        let n = u32::try_from(expected.len())?;
        assert_eq!(writer.read_vals(n).await?, expected, "{types}");
    }
    Ok(())
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn guest_stream_read_vals_stops_at_invalid_item() -> Result<()> {
    // The second string is not valid UTF-8.
    let data = r#"(data (i32.const 100) "\c8\00\00\00\01\00\00\00\c9\00\00\00\01\00\00\00")
                  (data (i32.const 200) "a\ff")"#;
    let mut writer = StreamWriter::new("(type $s (stream string))", data).await?;
    let (consumer, done) = Collect::vals();
    writer.stream.pipe(&mut writer.store, consumer)?;
    assert!(
        writer
            .write
            .call_async(&mut writer.store, (2,))
            .await
            .is_err()
    );
    drop(writer.store);
    assert_eq!(done.await?, [Val::String("a".into())]);
    Ok(())
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn payload_less_guest_stream_refuses_vals() -> Result<()> {
    let mut writer = StreamWriter::new("(type $s (stream))", "").await?;
    assert!(writer.stream.ty().unwrap().ty().is_none());
    let (consumer, _done) = Collect::vals();
    let err = writer
        .stream
        .clone()
        .pipe(&mut writer.store, consumer)
        .unwrap_err();
    assert!(err.to_string().contains("payload-less"), "{err:?}");
    writer.stream.close(&mut writer.store)?;
    Ok(())
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn typed_host_stream_refuses_vals() -> Result<()> {
    let (mut store, instance) = instantiate_echo().await?;

    let mut stream =
        StreamReader::new(&mut store, vec!["a".to_string()])?.try_into_stream_any(&mut store)?;
    assert!(stream.ty().is_none());
    let (consumer, _done) = Collect::vals();
    let err = stream.clone().pipe(&mut store, consumer).unwrap_err();
    assert!(err.to_string().contains("both use `Val` items"), "{err:?}");
    stream.close(&mut store)?;

    // Still refused after passing through a guest, which gives it a
    // component model type.
    let echo =
        instance.get_typed_func::<(StreamReader<String>,), (StreamAny,)>(&mut store, "echo-s")?;
    let reader = StreamReader::new(&mut store, vec!["a".to_string()])?;
    let (mut stream,) = echo.call_async(&mut store, (reader,)).await?;
    assert!(stream.ty().is_some());
    let (consumer, _done) = Collect::vals();
    let err = stream.clone().pipe(&mut store, consumer).unwrap_err();
    assert!(err.to_string().contains("both use `Val` items"), "{err:?}");
    stream.close(&mut store)?;
    Ok(())
}

/// Returns a component whose async `forward` export takes a stream of type
/// `$s`, defined by `types`, and returns a new stream of the same type to
/// which it copies every item of the first, four at a time.
fn stream_forwarder(types: &str) -> String {
    format!(
        r#"
(component
  {types}
  (core module $libc
    (memory (export "mem") 1)
    (global $heap (mut i32) (i32.const 4096))
    (func (export "realloc") (param i32 i32 i32 i32) (result i32)
      (local $ret i32)
      (local.set $ret
        (i32.and
          (i32.add (global.get $heap) (i32.sub (local.get 2) (i32.const 1)))
          (i32.sub (i32.const 0) (local.get 2))))
      (global.set $heap (i32.add (local.get $ret) (local.get 3)))
      (local.get $ret)))
  (core instance $libc (instantiate $libc))
  (core func $stream.new (canon stream.new $s))
  (core func $stream.read
    (canon stream.read $s async (memory (core memory $libc "mem"))
      (realloc (core func $libc "realloc"))))
  (core func $stream.write
    (canon stream.write $s async (memory (core memory $libc "mem"))))
  (core func $stream.drop-readable (canon stream.drop-readable $s))
  (core func $stream.drop-writable (canon stream.drop-writable $s))
  (core func $task.return (canon task.return (result $s)))
  (core func $waitable-set.new (canon waitable-set.new))
  (core func $waitable.join (canon waitable.join))
  (core module $m
    (import "" "stream.new" (func $stream.new (result i64)))
    (import "" "stream.read" (func $stream.read (param i32 i32 i32) (result i32)))
    (import "" "stream.write" (func $stream.write (param i32 i32 i32) (result i32)))
    (import "" "stream.drop-readable" (func $stream.drop-readable (param i32)))
    (import "" "stream.drop-writable" (func $stream.drop-writable (param i32)))
    (import "" "task.return" (func $task.return (param i32)))
    (import "" "waitable-set.new" (func $waitable-set.new (result i32)))
    (import "" "waitable.join" (func $waitable.join (param i32 i32)))

    (global $in (mut i32) (i32.const 0))
    (global $out (mut i32) (i32.const 0))
    (global $ws (mut i32) (i32.const 0))
    ;; Whether `$in` has been dropped by its writer.
    (global $in-dropped (mut i32) (i32.const 0))

    ;; Waits for `$handle`, returning CALLBACK_CODE_WAIT for `$ws`.
    (func $wait (param $handle i32) (result i32)
      (call $waitable.join (local.get $handle) (global.get $ws))
      (i32.or (i32.const 2) (i32.shl (global.get $ws) (i32.const 4))))

    (func $read (result i32)
      (local $code i32)
      (local.set $code
        (call $stream.read (global.get $in) (i32.const 1024) (i32.const 4)))
      (if (i32.eq (local.get $code) (i32.const -1))
        (then (return (call $wait (global.get $in)))))
      (call $on-read (local.get $code)))

    ;; Writes what a read delivered, or finishes once `$in` is dropped.
    (func $on-read (param $code i32) (result i32)
      (local $n i32)
      (global.set $in-dropped (i32.eq (i32.and (local.get $code) (i32.const 0xf)) (i32.const 1)))
      (local.set $n (i32.shr_u (local.get $code) (i32.const 4)))
      (if (i32.eqz (local.get $n))
        (then (return (call $after-write))))
      (local.set $code
        (call $stream.write (global.get $out) (i32.const 1024) (local.get $n)))
      (if (i32.eq (local.get $code) (i32.const -1))
        (then (return (call $wait (global.get $out)))))
      (call $on-write (local.get $code)))

    (func $on-write (param $code i32) (result i32)
      ;; The host consumer takes everything, so anything else is a bug here.
      (if (i32.ne (i32.and (local.get $code) (i32.const 0xf)) (i32.const 0))
        (then unreachable))
      (call $after-write))

    (func $after-write (result i32)
      (if (i32.eqz (global.get $in-dropped))
        (then (return (call $read))))
      (call $stream.drop-readable (global.get $in))
      (call $stream.drop-writable (global.get $out))
      (i32.const 0)) ;; CALLBACK_CODE_EXIT

    (func (export "forward") (param $in i32) (result i32)
      (local $rw i64)
      (global.set $in (local.get $in))
      (local.set $rw (call $stream.new))
      (global.set $out (i32.wrap_i64 (i64.shr_u (local.get $rw) (i64.const 32))))
      (global.set $ws (call $waitable-set.new))
      (call $task.return (i32.wrap_i64 (local.get $rw)))
      (call $read))

    (func (export "cb") (param $event i32) (param $handle i32) (param $code i32) (result i32)
      (call $waitable.join (local.get $handle) (i32.const 0))
      (if (i32.eq (local.get $handle) (global.get $in))
        (then (return (call $on-read (local.get $code)))))
      (call $on-write (local.get $code))))
  (core instance $i (instantiate $m (with "" (instance
    (export "stream.new" (func $stream.new))
    (export "stream.read" (func $stream.read))
    (export "stream.write" (func $stream.write))
    (export "stream.drop-readable" (func $stream.drop-readable))
    (export "stream.drop-writable" (func $stream.drop-writable))
    (export "task.return" (func $task.return))
    (export "waitable-set.new" (func $waitable-set.new))
    (export "waitable.join" (func $waitable.join))))))
  (func (export "forward") async (param "s" $s) (result $s)
    (canon lift (core func $i "forward") async (callback (core func $i "cb"))))
)
"#
    )
}

/// A host resource type which `stream_forwarder` components may import as
/// `r`.
struct HostThing;

/// Instantiates `stream_forwarder(types)`, returning its store, the type of
/// stream it forwards, and its `forward` export.
async fn forwarder(
    types: &str,
) -> Result<(Store<()>, StreamType, TypedFunc<(StreamAny,), (StreamAny,)>)> {
    let mut config = Config::new();
    config.wasm_component_model_async(true);
    let engine = Engine::new(&config)?;
    let component = Component::new(&engine, stream_forwarder(types))?;
    let mut store = Store::new(&engine, ());
    let mut linker = Linker::new(&engine);
    linker
        .root()
        .resource("r", ResourceType::host::<HostThing>(), |_, _| Ok(()))?;
    let instance = linker.instantiate_async(&mut store, &component).await?;
    let func = instance.get_func(&mut store, "forward").unwrap();
    let Type::Stream(ty) = func.ty(&store).params().next().unwrap().1 else {
        unreachable!()
    };
    let forward = func.typed::<(StreamAny,), (StreamAny,)>(&store)?;
    Ok((store, ty, forward))
}

/// Writes `items` to a new `Val` stream of type `ty`, passes it to `forward`,
/// and reads what comes back as `Val`s.
async fn forward_vals(
    store: &mut Store<()>,
    ty: StreamType,
    forward: &TypedFunc<(StreamAny,), (StreamAny,)>,
    items: Vec<Val>,
) -> Result<Vec<Val>> {
    let stream = StreamAny::new(&mut *store, ty, items)?;
    let (stream,) = forward.call_async(&mut *store, (stream,)).await?;
    let (consumer, done) = Collect::vals();
    stream.pipe(&mut *store, consumer)?;
    Ok(store.run_concurrent(async |_| done.await).await??)
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn val_stream_round_trips_through_guest() -> Result<()> {
    let string = |s: &str| Val::String(s.into());
    let cases = [
        ("(type $s (stream u32))", (1..=6).map(Val::U32).collect()),
        (
            "(type $s (stream string))",
            vec![
                string("a"),
                string(""),
                string("long enough to need realloc"),
            ],
        ),
        (
            r#"(type $r (record (field "a" u32) (field "b" string)))
               (export $r' "r" (type $r))
               (type $s (stream $r'))"#,
            vec![
                record(&[("a", Val::U32(1)), ("b", string("x"))]),
                record(&[("a", Val::U32(2)), ("b", string("yz"))]),
            ],
        ),
        (
            "(type $s (stream (option string)))",
            vec![Val::Option(Some(Box::new(string("x")))), Val::Option(None)],
        ),
        (
            r#"(type $v (variant (case "a" u8) (case "b" string) (case "c")))
               (export $v' "v" (type $v))
               (type $s (stream $v'))"#,
            vec![
                Val::Variant("b".into(), Some(Box::new(string("x")))),
                Val::Variant("a".into(), Some(Box::new(Val::U8(3)))),
                Val::Variant("c".into(), None),
            ],
        ),
        (
            "(type $s (stream (list u16)))",
            vec![Val::List(vec![Val::U16(1), Val::U16(2)]), Val::List(vec![])],
        ),
    ];
    for (types, items) in cases {
        let (mut store, ty, forward) = forwarder(types).await?;
        assert_eq!(
            forward_vals(&mut store, ty, &forward, items.clone()).await?,
            items,
            "{types}"
        );
    }
    Ok(())
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn val_stream_moves_resources_through_guest() -> Result<()> {
    let (mut store, ty, forward) = forwarder(
        r#"(import "r" (type $r (sub resource)))
           (type $s (stream (own $r)))"#,
    )
    .await?;
    let items = [7, 8]
        .into_iter()
        .map(|rep| {
            let r = Resource::<HostThing>::new_own(rep).try_into_resource_any(&mut store)?;
            Ok(Val::Resource(r))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut reps = Vec::new();
    for item in forward_vals(&mut store, ty, &forward, items).await? {
        let Val::Resource(r) = item else {
            panic!("expected a resource, found {item:?}")
        };
        reps.push(r.try_into_resource::<HostThing>(&mut store)?.rep());
    }
    assert_eq!(reps, [7, 8]);
    Ok(())
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn val_stream_items_are_typechecked_when_lowered() -> Result<()> {
    let (mut store, ty, forward) = forwarder("(type $s (stream u32))").await?;
    let items = vec![Val::U32(1), Val::String("not a u32".into())];
    let err = forward_vals(&mut store, ty, &forward, items)
        .await
        .unwrap_err();
    assert!(
        format!("{err:?}").contains("type mismatch: expected u32, found string"),
        "{err:?}"
    );
    Ok(())
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn val_stream_read_host_to_host() -> Result<()> {
    let (mut store, ty, _) = forwarder("(type $s (stream string))").await?;
    let items = vec![Val::String("a".into()), Val::String("b".into())];
    let stream = StreamAny::new(&mut store, ty.clone(), items.clone())?;
    assert_eq!(stream.ty(), Some(ty));
    let (consumer, done) = Collect::vals();
    stream.pipe(&mut store, consumer)?;
    assert_eq!(store.run_concurrent(async |_| done.await).await??, items);
    Ok(())
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn val_stream_refuses_typed_consumer() -> Result<()> {
    let (mut store, ty, _) = forwarder("(type $s (stream u32))").await?;
    let mut stream = StreamAny::new(&mut store, ty, vec![Val::U32(1)])?;
    let reader = stream.clone().try_into_stream_reader::<u32>()?;
    let err = reader
        .pipe(&mut store, Collect::<u32>::new().0)
        .unwrap_err();
    assert!(err.to_string().contains("both use `Val` items"), "{err:?}");
    stream.close(&mut store)?;
    Ok(())
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn payload_less_val_stream_refused() -> Result<()> {
    let (mut store, ty, _) = forwarder("(type $s (stream))").await?;
    let err = StreamAny::new(&mut store, ty, Vec::<Val>::new()).unwrap_err();
    assert!(err.to_string().contains("payload-less"), "{err:?}");
    Ok(())
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn val_stream_lowering_failure_keeps_moved_resources_moved() -> Result<()> {
    let (mut store, ty, forward) = forwarder(
        r#"(import "r" (type $r (sub resource)))
           (type $s (stream (own $r)))"#,
    )
    .await?;
    let sent = Resource::<HostThing>::new_own(7).try_into_resource_any(&mut store)?;
    let kept = Resource::<HostThing>::new_own(8).try_into_resource_any(&mut store)?;
    let items = vec![Val::Resource(sent), Val::String("not a resource".into())];
    assert!(forward_vals(&mut store, ty, &forward, items).await.is_err());

    // The first item was moved into the guest before the second failed.
    assert!(sent.try_into_resource::<HostThing>(&mut store).is_err());
    assert_eq!(kept.try_into_resource::<HostThing>(&mut store)?.rep(), 8);
    Ok(())
}

/// A producer of no `Val`s which can be recovered with `StreamAny::try_into`.
struct Tagged(u32);

impl StreamProducer<()> for Tagged {
    type Item = Val;
    type Buffer = VecBuffer<Val>;

    fn poll_produce<'a>(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _store: StoreContextMut<'a, ()>,
        _dst: Destination<'a, Val, Self::Buffer>,
        _finish: bool,
    ) -> Poll<Result<StreamResult>> {
        Poll::Ready(Ok(StreamResult::Dropped))
    }

    fn try_into(me: Pin<Box<Self>>, ty: TypeId) -> Result<Box<dyn Any>, Pin<Box<Self>>> {
        if ty == TypeId::of::<Tagged>() {
            let me: Box<Self> = Pin::into_inner(me);
            Ok(me)
        } else {
            Err(me)
        }
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn val_streams_returned_by_guest_recover_their_producers() -> Result<()> {
    let (mut store, instance) = instantiate_echo().await?;
    let echo = instance.get_func(&mut store, "echo-s").unwrap();
    let Type::Stream(ty) = echo.ty(&store).params().next().unwrap().1 else {
        unreachable!()
    };
    let echo = echo.typed::<(StreamAny,), (StreamAny,)>(&store)?;
    let a = StreamAny::new(&mut store, ty.clone(), Tagged(1))?;
    let b = StreamAny::new(&mut store, ty, Tagged(2))?;

    // Streams of the same type come back in a different order, as new
    // `StreamAny`s, and are still told apart.
    let (b,) = echo.call_async(&mut store, (b,)).await?;
    let (a,) = echo.call_async(&mut store, (a,)).await?;

    let a = match a.try_into::<String>(&mut store) {
        Ok(_) => panic!("`Tagged` does not convert into `String`"),
        Err(a) => a,
    };
    assert_eq!(b.try_into::<Tagged>(&mut store).ok().unwrap().0, 2);
    assert_eq!(a.try_into::<Tagged>(&mut store).ok().unwrap().0, 1);
    Ok(())
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn guest_stream_cannot_be_converted_into_a_producer() -> Result<()> {
    let mut writer = StreamWriter::new("(type $s (stream u32))", "").await?;
    let mut stream = match writer.stream.clone().try_into::<Tagged>(&mut writer.store) {
        Ok(_) => panic!("a guest stream has no host producer"),
        Err(stream) => stream,
    };
    stream.close(&mut writer.store)?;
    Ok(())
}
