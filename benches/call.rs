use criterion::measurement::WallTime;
use criterion::{BenchmarkGroup, Criterion, criterion_group, criterion_main};
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
use std::time::Instant;
use wasmtime::*;

criterion_main!(benches);
criterion_group!(benches, measure_execution_time);

fn measure_execution_time(c: &mut Criterion) {
    host_to_wasm(c);
    wasm_to_host(c);

    #[cfg(feature = "component-model")]
    component::measure_execution_time(c);

    #[cfg(feature = "component-model-async")]
    component_async::measure_execution_time(c);

    #[cfg(feature = "component-model-async")]
    component_streams::measure_execution_time(c);

    indirect::measure_execution_time(c);
}

#[derive(Copy, Clone)]
enum IsAsync {
    Yes,
    YesPooling,
    No,
    NoPooling,
}

impl IsAsync {
    fn desc(&self) -> &str {
        match self {
            IsAsync::Yes => "async",
            IsAsync::YesPooling => "async-pool",
            IsAsync::No => "sync",
            IsAsync::NoPooling => "sync-pool",
        }
    }
    fn use_async(&self) -> bool {
        match self {
            IsAsync::Yes | IsAsync::YesPooling => true,
            IsAsync::No | IsAsync::NoPooling => false,
        }
    }
}

fn engines(concurrency_support: bool) -> Vec<(Engine, IsAsync)> {
    let mut config = Config::new();

    #[cfg(feature = "component-model")]
    config.wasm_component_model(true);

    #[cfg(feature = "component-model-async")]
    config.concurrency_support(concurrency_support);

    let mut pool = PoolingAllocationConfig::default();
    if std::env::var("WASMTIME_TEST_FORCE_MPK").is_ok() {
        pool.memory_protection_keys(Enabled::Yes);
    }

    vec![
        (Engine::new(&config).unwrap(), IsAsync::No),
        (
            Engine::new(
                config
                    .clone()
                    .allocation_strategy(InstanceAllocationStrategy::Pooling(pool.clone())),
            )
            .unwrap(),
            IsAsync::NoPooling,
        ),
        (Engine::new(&config).unwrap(), IsAsync::Yes),
        (
            Engine::new(config.allocation_strategy(InstanceAllocationStrategy::Pooling(pool)))
                .unwrap(),
            IsAsync::YesPooling,
        ),
    ]
}

/// Benchmarks the overhead of calling WebAssembly from the host in various
/// configurations.
fn host_to_wasm(c: &mut Criterion) {
    for (engine, is_async) in engines(false) {
        let mut store = Store::new(&engine, ());
        let module = Module::new(
            &engine,
            r#"(module
                (func (export "nop"))
                (func (export "nop-params-and-results") (param i32 i64) (result f32)
                    f32.const 0)
            )"#,
        )
        .unwrap();
        let instance = if is_async.use_async() {
            run_await(Instance::new_async(&mut store, &module, &[])).unwrap()
        } else {
            Instance::new(&mut store, &module, &[]).unwrap()
        };

        let bench_calls = |group: &mut BenchmarkGroup<'_, WallTime>, store: &mut Store<()>| {
            // Bench the overhead of a function that has no parameters or results
            bench_host_to_wasm::<(), ()>(group, store, &instance, is_async, "nop", (), ());
            // Bench the overhead of a function that has some parameters and just
            // one result (will use the raw system-v convention on applicable
            // platforms).
            bench_host_to_wasm::<(i32, i64), (f32,)>(
                group,
                store,
                &instance,
                is_async,
                "nop-params-and-results",
                (0, 0),
                (0.0,),
            );
        };

        // Bench once without any call hooks configured
        let name = format!("{}/no-hook", is_async.desc());
        bench_calls(&mut c.benchmark_group(&name), &mut store);

        // Bench again with a "call hook" enabled
        store.call_hook(|_, _| Ok(()));
        let name = format!("{}/hook-sync", is_async.desc());
        bench_calls(&mut c.benchmark_group(&name), &mut store);
    }
}

fn bench_host_to_wasm<Params, Results>(
    c: &mut BenchmarkGroup<'_, WallTime>,
    store: &mut Store<()>,
    instance: &Instance,
    is_async: IsAsync,
    name: &str,
    typed_params: Params,
    typed_results: Results,
) where
    Params: WasmParams + ToVals + Copy + Sync,
    Results: WasmResults + ToVals + Copy + Sync + PartialEq + Debug + 'static,
{
    // Benchmark the "typed" version, which should be faster than the versions
    // below.
    c.bench_function(&format!("core - host-to-wasm - typed - {name}"), |b| {
        let typed = instance
            .get_typed_func::<Params, Results>(&mut *store, name)
            .unwrap();
        b.iter(|| {
            let results = if is_async.use_async() {
                run_await(typed.call_async(&mut *store, typed_params)).unwrap()
            } else {
                typed.call(&mut *store, typed_params).unwrap()
            };
            assert_eq!(results, typed_results);
        })
    });

    // Benchmark the "untyped" version which should be the slowest of the three
    // here, but not unduly slow.
    c.bench_function(&format!("core - host-to-wasm - untyped - {name}"), |b| {
        let untyped = instance.get_func(&mut *store, name).unwrap();
        let params = typed_params.to_vals();
        let expected_results = typed_results.to_vals();
        let mut results = vec![Val::I32(0); expected_results.len()];
        b.iter(|| {
            if is_async.use_async() {
                run_await(untyped.call_async(&mut *store, &params, &mut results)).unwrap();
            } else {
                untyped.call(&mut *store, &params, &mut results).unwrap();
            }
            for (expected, actual) in expected_results.iter().zip(&results) {
                assert_vals_eq(expected, actual);
            }
        })
    });

    // Currently `call_async_unchecked` isn't implemented, so can't benchmark
    // below
    if is_async.use_async() {
        return;
    }

    // Benchmark the "unchecked" version which should be between the above two,
    // but is unsafe.
    c.bench_function(&format!("core - host-to-wasm - unchecked - {name}"), |b| {
        let untyped = instance.get_func(&mut *store, name).unwrap();
        let params = typed_params.to_vals();
        let results = typed_results.to_vals();
        let mut space = vec![ValRaw::i32(0); params.len().max(results.len())];
        b.iter(|| unsafe {
            for (i, param) in params.iter().enumerate() {
                space[i] = param.to_raw(&mut *store).unwrap();
            }
            untyped.call_unchecked(&mut *store, &mut space[..]).unwrap();
            for (i, expected) in results.iter().enumerate() {
                let ty = expected.ty(&store).unwrap();
                let actual = Val::from_raw(&mut *store, space[i], ty);
                assert_vals_eq(expected, &actual);
            }
        })
    });
}

/// Benchmarks the overhead of calling the host from WebAssembly itself
fn wasm_to_host(c: &mut Criterion) {
    let module = r#"(module
        ;; host imports with a variety of parameters/arguments
        (import "" "nop" (func $nop))
        (import "" "nop-params-and-results"
            (func $nop_params_and_results (param i32 i64) (result f32))
        )

        ;; "runner functions" for each of the above imports. Each runner
        ;; function takes the number of times to call the host function as
        ;; the duration of this entire loop will be measured.

        (func (export "run-nop") (param i64)
            loop
                call $nop

                local.get 0             ;; decrement & break if necessary
                i64.const -1
                i64.add
                local.tee 0
                i64.const 0
                i64.ne
                br_if 0
            end
        )

        (func (export "run-nop-params-and-results") (param i64)
            loop
                i32.const 0             ;; always zero parameters
                i64.const 0
                call $nop_params_and_results
                f32.const 0             ;; assert the correct result
                f32.eq
                i32.eqz
                if
                    unreachable
                end

                local.get 0             ;; decrement & break if necessary
                i64.const -1
                i64.add
                local.tee 0
                i64.const 0
                i64.ne
                br_if 0
            end
        )

    )"#;

    for (engine, is_async) in engines(false) {
        let mut store = Store::new(&engine, ());
        let module = Module::new(&engine, module).unwrap();

        bench_calls(
            &mut c.benchmark_group(&format!("{}/no-hook", is_async.desc())),
            &mut store,
            &module,
            is_async,
        );
        store.call_hook(|_, _| Ok(()));
        bench_calls(
            &mut c.benchmark_group(&format!("{}/hook-sync", is_async.desc())),
            &mut store,
            &module,
            is_async,
        );
    }

    // Given a `Store` will create various instances hooked up to different ways
    // of defining host imports to benchmark their overhead.
    fn bench_calls(
        group: &mut BenchmarkGroup<'_, WallTime>,
        store: &mut Store<()>,
        module: &Module,
        is_async: IsAsync,
    ) {
        let engine = store.engine().clone();
        let mut typed = Linker::new(&engine);
        typed.func_wrap("", "nop", || {}).unwrap();
        typed
            .func_wrap("", "nop-params-and-results", |x: i32, y: i64| {
                assert_eq!(x, 0);
                assert_eq!(y, 0);
                0.0f32
            })
            .unwrap();
        let instance = if is_async.use_async() {
            run_await(typed.instantiate_async(&mut *store, &module)).unwrap()
        } else {
            typed.instantiate(&mut *store, &module).unwrap()
        };
        bench_instance(group, store, &instance, "typed", is_async);

        let mut untyped = Linker::new(&engine);
        untyped
            .func_new("", "nop", FuncType::new(&engine, [], []), |_, _, _| Ok(()))
            .unwrap();
        let ty = FuncType::new(&engine, [ValType::I32, ValType::I64], [ValType::F32]);
        untyped
            .func_new(
                "",
                "nop-params-and-results",
                ty,
                |_caller, params, results| {
                    assert_eq!(params.len(), 2);
                    match params[0] {
                        Val::I32(0) => {}
                        _ => unreachable!(),
                    }
                    match params[1] {
                        Val::I64(0) => {}
                        _ => unreachable!(),
                    }
                    assert_eq!(results.len(), 1);
                    results[0] = Val::F32(0);
                    Ok(())
                },
            )
            .unwrap();
        let instance = if is_async.use_async() {
            run_await(untyped.instantiate_async(&mut *store, &module)).unwrap()
        } else {
            untyped.instantiate(&mut *store, &module).unwrap()
        };
        bench_instance(group, store, &instance, "untyped", is_async);

        unsafe {
            let mut unchecked = Linker::new(&engine);
            unchecked
                .func_new_unchecked("", "nop", FuncType::new(&engine, [], []), |_, _| Ok(()))
                .unwrap();
            let ty = FuncType::new(&engine, [ValType::I32, ValType::I64], [ValType::F32]);
            unchecked
                .func_new_unchecked("", "nop-params-and-results", ty, |mut caller, space| {
                    match Val::from_raw(&mut caller, space[0].assume_init(), ValType::I32) {
                        Val::I32(0) => {}
                        _ => unreachable!(),
                    }
                    match Val::from_raw(&mut caller, space[1].assume_init(), ValType::I64) {
                        Val::I64(0) => {}
                        _ => unreachable!(),
                    }
                    space[0].write(Val::F32(0).to_raw(&mut caller).unwrap());
                    Ok(())
                })
                .unwrap();
            let instance = if is_async.use_async() {
                run_await(unchecked.instantiate_async(&mut *store, &module)).unwrap()
            } else {
                unchecked.instantiate(&mut *store, &module).unwrap()
            };
            bench_instance(group, store, &instance, "unchecked", is_async);
        }

        // Only define async host imports if allowed
        if !is_async.use_async() {
            return;
        }

        let mut typed = Linker::<()>::new(&engine);
        typed
            .func_wrap_async("", "nop", |caller, _: ()| {
                Box::new(async {
                    drop(caller);
                })
            })
            .unwrap();
        typed
            .func_wrap_async(
                "",
                "nop-params-and-results",
                |_caller, (x, y): (i32, i64)| {
                    Box::new(async move {
                        assert_eq!(x, 0);
                        assert_eq!(y, 0);
                        0.0f32
                    })
                },
            )
            .unwrap();
        let instance = run_await(typed.instantiate_async(&mut *store, &module)).unwrap();
        bench_instance(group, store, &instance, "async-typed", is_async);
    }

    // Given a specific instance executes all of the "runner functions"
    fn bench_instance(
        group: &mut BenchmarkGroup<'_, WallTime>,
        store: &mut Store<()>,
        instance: &Instance,
        desc: &str,
        is_async: IsAsync,
    ) {
        group.bench_function(&format!("core - wasm-to-host - {desc} - nop"), |b| {
            let run = instance
                .get_typed_func::<u64, ()>(&mut *store, "run-nop")
                .unwrap();
            b.iter_custom(|iters| {
                let start = Instant::now();
                if is_async.use_async() {
                    run_await(run.call_async(&mut *store, iters)).unwrap();
                } else {
                    run.call(&mut *store, iters).unwrap();
                }
                start.elapsed()
            })
        });
        group.bench_function(
            &format!("core - wasm-to-host - {desc} - nop-params-and-results"),
            |b| {
                let run = instance
                    .get_typed_func::<u64, ()>(&mut *store, "run-nop-params-and-results")
                    .unwrap();
                b.iter_custom(|iters| {
                    let start = Instant::now();
                    if is_async.use_async() {
                        run_await(run.call_async(&mut *store, iters)).unwrap();
                    } else {
                        run.call(&mut *store, iters).unwrap();
                    }
                    start.elapsed()
                })
            },
        );
    }
}

fn assert_vals_eq(a: &Val, b: &Val) {
    match (a, b) {
        (Val::I32(a), Val::I32(b)) => assert_eq!(a, b),
        (Val::I64(a), Val::I64(b)) => assert_eq!(a, b),
        (Val::F32(a), Val::F32(b)) => assert_eq!(a, b),
        (Val::F64(a), Val::F64(b)) => assert_eq!(a, b),
        _ => unimplemented!(),
    }
}

trait ToVals {
    fn to_vals(&self) -> Vec<Val>;
}

macro_rules! tuples {
    ($($t:ident)*) => (
        #[allow(non_snake_case, reason = "macro-generated code")]
        impl<$($t:Copy + Into<Val>,)*> ToVals for ($($t,)*) {
            fn to_vals(&self) -> Vec<Val> {
                let mut _dst = Vec::new();
                let ($($t,)*) = *self;
                $(_dst.push($t.into());)*
                _dst
            }
        }
    )
}

tuples!();
tuples!(A);
tuples!(A B);
tuples!(A B C);

fn run_await<F: Future>(future: F) -> F::Output {
    let mut f = Pin::from(Box::new(future));
    let waker = dummy_waker();
    let mut cx = Context::from_waker(&waker);
    loop {
        match f.as_mut().poll(&mut cx) {
            Poll::Ready(val) => break val,
            Poll::Pending => {}
        }
    }
}

fn dummy_waker() -> Waker {
    return unsafe { Waker::from_raw(clone(5 as *const _)) };

    unsafe fn clone(ptr: *const ()) -> RawWaker {
        assert_eq!(ptr as usize, 5);
        const VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, drop);
        RawWaker::new(ptr, &VTABLE)
    }

    unsafe fn wake(ptr: *const ()) {
        assert_eq!(ptr as usize, 5);
    }

    unsafe fn wake_by_ref(ptr: *const ()) {
        assert_eq!(ptr as usize, 5);
    }

    unsafe fn drop(ptr: *const ()) {
        assert_eq!(ptr as usize, 5);
    }
}

#[cfg(feature = "component-model")]
mod component {
    use super::*;
    use wasmtime::component::{self, Component};

    pub fn measure_execution_time(c: &mut Criterion) {
        host_to_wasm(c);
        wasm_to_host(c);
    }

    trait ToComponentVal {
        fn to_component_val(&self) -> component::Val;
    }

    impl ToComponentVal for u32 {
        fn to_component_val(&self) -> component::Val {
            component::Val::U32(*self)
        }
    }

    impl ToComponentVal for u64 {
        fn to_component_val(&self) -> component::Val {
            component::Val::U64(*self)
        }
    }

    impl ToComponentVal for f32 {
        fn to_component_val(&self) -> component::Val {
            component::Val::Float32(*self)
        }
    }

    trait ToComponentVals {
        fn to_component_vals(&self) -> Vec<component::Val>;
    }

    macro_rules! tuples {
        ($($t:ident)*) => (
            #[allow(non_snake_case, reason = "macro-generated code")]
            impl<$($t:Copy + ToComponentVal,)*> ToComponentVals for ($($t,)*) {
                fn to_component_vals(&self) -> Vec<component::Val> {
                    let mut _dst = Vec::new();
                    let ($($t,)*) = *self;
                    $(_dst.push($t.to_component_val());)*
                    _dst
                }
            }
        )
    }

    tuples!();
    tuples!(A);
    tuples!(A B);
    tuples!(A B C);

    fn engines() -> Vec<(String, Engine, IsAsync)> {
        let mut result: Vec<_> = super::engines(false)
            .into_iter()
            .map(|(e, a)| ("no-concurrent".to_string(), e, a))
            .collect();
        #[cfg(feature = "component-model-async")]
        result.extend(
            super::engines(true)
                .into_iter()
                .map(|(e, a)| ("concurrent".to_string(), e, a)),
        );
        result
    }

    fn host_to_wasm(c: &mut Criterion) {
        for (concurrent, engine, is_async) in engines() {
            let mut store = Store::new(&engine, ());

            let component = Component::new(
                &engine,
                r#"
                    (component
                        (core module $m
                            (func (export "nop"))
                            (func (export "nop-params-and-results") (param i32 i64) (result f32)
                                f32.const 0
                            )
                        )
                        (core instance $i (instantiate $m))
                        (func (export "nop")
                            (canon lift (core func $i "nop"))
                        )
                        (func (export "nop-params-and-results") (param "x" u32) (param "y" u64) (result float32)
                            (canon lift (core func $i "nop-params-and-results"))
                        )
                    )
                "#,
            )
            .unwrap();

            let linker = component::Linker::<()>::new(&engine);
            let instance = if is_async.use_async() {
                run_await(linker.instantiate_async(&mut store, &component)).unwrap()
            } else {
                linker.instantiate(&mut store, &component).unwrap()
            };

            let bench_calls = |group: &mut BenchmarkGroup<'_, WallTime>, store: &mut Store<()>| {
                // Bench the overhead of a function that has no parameters or results
                bench_host_to_wasm::<(), ()>(group, store, &instance, is_async, "nop", (), ());
                // Bench the overhead of a function that has some parameters and just
                // one result (will use the raw system-v convention on applicable
                // platforms).
                bench_host_to_wasm::<(u32, u64), (f32,)>(
                    group,
                    store,
                    &instance,
                    is_async,
                    "nop-params-and-results",
                    (0, 0),
                    (0.0,),
                );
            };

            // Bench once without any call hooks configured
            let name = format!("{}/{}/no-hook", concurrent, is_async.desc());
            bench_calls(&mut c.benchmark_group(&name), &mut store);

            // Bench again with a "call hook" enabled
            store.call_hook(|_, _| Ok(()));
            let name = format!("{}/{}/hook-sync", concurrent, is_async.desc());
            bench_calls(&mut c.benchmark_group(&name), &mut store);
        }
    }

    fn bench_host_to_wasm<Params, Results>(
        c: &mut BenchmarkGroup<'_, WallTime>,
        store: &mut Store<()>,
        instance: &component::Instance,
        is_async: IsAsync,
        name: &str,
        typed_params: Params,
        typed_results: Results,
    ) where
        Params:
            component::ComponentNamedList + ToComponentVals + component::Lower + Copy + Send + Sync,
        Results: component::ComponentNamedList
            + ToComponentVals
            + component::Lift
            + Copy
            + PartialEq
            + Debug
            + Send
            + Sync
            + 'static,
    {
        // Benchmark the "typed" version.
        c.bench_function(&format!("component - host-to-wasm - typed - {name}"), |b| {
            let typed = instance
                .get_typed_func::<Params, Results>(&mut *store, name)
                .unwrap();
            b.iter(|| {
                let results = if is_async.use_async() {
                    run_await(typed.call_async(&mut *store, typed_params)).unwrap()
                } else {
                    typed.call(&mut *store, typed_params).unwrap()
                };
                assert_eq!(results, typed_results);
            })
        });

        // Benchmark the "untyped" version.
        c.bench_function(
            &format!("component - host-to-wasm - untyped - {name}"),
            |b| {
                let untyped = instance.get_func(&mut *store, name).unwrap();
                let params = typed_params.to_component_vals();
                let expected_results = typed_results.to_component_vals();
                let mut results = vec![component::Val::U32(0); expected_results.len()];
                b.iter(|| {
                    if is_async.use_async() {
                        run_await(untyped.call_async(&mut *store, &params, &mut results)).unwrap();
                    } else {
                        untyped.call(&mut *store, &params, &mut results).unwrap();
                    }
                    for (expected, actual) in expected_results.iter().zip(&results) {
                        assert_eq!(expected, actual);
                    }
                })
            },
        );
    }

    fn wasm_to_host(c: &mut Criterion) {
        let module = r#"
            (component
                (import "nop" (func $comp_nop))
                (import "nop-params-and-results" (func $comp_nop_params_and_results (param "x" u32) (param "y" u64) (result float32)))

                (core func $core_nop (canon lower (func $comp_nop)))
                (core func $core_nop_params_and_results (canon lower (func $comp_nop_params_and_results)))

                (core module $m
                    ;; host imports with a variety of parameters/arguments
                    (import "" "nop" (func $nop))
                    (import "" "nop-params-and-results"
                        (func $nop_params_and_results (param i32 i64) (result f32))
                    )

                    ;; "runner functions" for each of the above imports. Each runner
                    ;; function takes the number of times to call the host function as
                    ;; the duration of this entire loop will be measured.

                    (func (export "run-nop") (param i64)
                        loop
                            call $nop

                            local.get 0             ;; decrement & break if necessary
                            i64.const -1
                            i64.add
                            local.tee 0
                            i64.const 0
                            i64.ne
                            br_if 0
                        end
                    )

                    (func (export "run-nop-params-and-results") (param i64)
                        loop
                            i32.const 0             ;; always zero parameters
                            i64.const 0
                            call $nop_params_and_results
                            f32.const 0             ;; assert the correct result
                            f32.eq
                            i32.eqz
                            if
                                unreachable
                            end

                            local.get 0             ;; decrement & break if necessary
                            i64.const -1
                            i64.add
                            local.tee 0
                            i64.const 0
                            i64.ne
                            br_if 0
                        end
                    )
                )

                (core instance $i (instantiate $m (with "" (instance
                  (export "nop" (func $core_nop))
                  (export "nop-params-and-results" (func $core_nop_params_and_results))
                ))))

                (func (export "run-nop") (param "i" u64)
                    (canon lift (core func $i "run-nop"))
                )
                (func (export "run-nop-params-and-results") (param "i" u64)
                    (canon lift (core func $i "run-nop-params-and-results"))
                )
            )
        "#;

        for (concurrent, engine, is_async) in engines() {
            let mut store = Store::new(&engine, ());
            let component = component::Component::new(&engine, module).unwrap();

            bench_calls(
                &mut c.benchmark_group(&format!("{}/{}/no-hook", concurrent, is_async.desc())),
                &mut store,
                &component,
                is_async,
            );
            store.call_hook(|_, _| Ok(()));
            bench_calls(
                &mut c.benchmark_group(&format!("{}/{}/hook-sync", concurrent, is_async.desc())),
                &mut store,
                &component,
                is_async,
            );
        }

        // Given a `Store` will create various instances hooked up to different ways
        // of defining host imports to benchmark their overhead.
        fn bench_calls(
            group: &mut BenchmarkGroup<'_, WallTime>,
            store: &mut Store<()>,
            component: &component::Component,
            is_async: IsAsync,
        ) {
            let engine = store.engine().clone();
            let mut typed = component::Linker::new(&engine);
            typed.root().func_wrap("nop", |_, ()| Ok(())).unwrap();
            typed
                .root()
                .func_wrap("nop-params-and-results", |_, (x, y): (u32, u64)| {
                    assert_eq!(x, 0);
                    assert_eq!(y, 0);
                    Ok((0.0f32,))
                })
                .unwrap();
            let instance = if is_async.use_async() {
                run_await(typed.instantiate_async(&mut *store, &component)).unwrap()
            } else {
                typed.instantiate(&mut *store, &component).unwrap()
            };
            bench_instance(group, store, &instance, "typed", is_async);

            let mut untyped = component::Linker::new(&engine);
            untyped.root().func_new("nop", |_, _, _, _| Ok(())).unwrap();
            untyped
                .root()
                .func_new("nop-params-and-results", |_caller, _ty, params, results| {
                    assert_eq!(params.len(), 2);
                    match params[0] {
                        component::Val::U32(0) => {}
                        _ => unreachable!(),
                    }
                    match params[1] {
                        component::Val::U64(0) => {}
                        _ => unreachable!(),
                    }
                    assert_eq!(results.len(), 1);
                    results[0] = component::Val::Float32(0.0);
                    Ok(())
                })
                .unwrap();
            let instance = if is_async.use_async() {
                run_await(untyped.instantiate_async(&mut *store, &component)).unwrap()
            } else {
                untyped.instantiate(&mut *store, &component).unwrap()
            };
            bench_instance(group, store, &instance, "untyped", is_async);

            // Only define async host imports if allowed
            if !is_async.use_async() {
                return;
            }

            let mut typed = component::Linker::new(&engine);
            typed
                .root()
                .func_wrap_async("nop", |caller, ()| {
                    Box::new(async {
                        drop(caller);
                        Ok(())
                    })
                })
                .unwrap();
            typed
                .root()
                .func_wrap_async("nop-params-and-results", |_caller, (x, y): (u32, u64)| {
                    Box::new(async move {
                        assert_eq!(x, 0);
                        assert_eq!(y, 0);
                        Ok((0.0f32,))
                    })
                })
                .unwrap();
            let instance = run_await(typed.instantiate_async(&mut *store, &component)).unwrap();
            bench_instance(group, store, &instance, "async-typed", is_async);
        }

        // Given a specific instance executes all of the "runner functions"
        fn bench_instance(
            group: &mut BenchmarkGroup<'_, WallTime>,
            store: &mut Store<()>,
            instance: &component::Instance,
            desc: &str,
            is_async: IsAsync,
        ) {
            group.bench_function(&format!("component - wasm-to-host - {desc} - nop"), |b| {
                let run = instance
                    .get_typed_func::<(u64,), ()>(&mut *store, "run-nop")
                    .unwrap();
                b.iter_custom(|iters| {
                    let start = Instant::now();
                    if is_async.use_async() {
                        run_await(run.call_async(&mut *store, (iters,))).unwrap();
                    } else {
                        run.call(&mut *store, (iters,)).unwrap();
                    }
                    start.elapsed()
                })
            });
            group.bench_function(
                &format!("component - wasm-to-host - {desc} - nop-params-and-results"),
                |b| {
                    let run = instance
                        .get_typed_func::<(u64,), ()>(&mut *store, "run-nop-params-and-results")
                        .unwrap();
                    b.iter_custom(|iters| {
                        let start = Instant::now();
                        if is_async.use_async() {
                            run_await(run.call_async(&mut *store, (iters,))).unwrap();
                        } else {
                            run.call(&mut *store, (iters,)).unwrap();
                        }
                        start.elapsed()
                    })
                },
            );
        }
    }
}

#[cfg(feature = "component-model-async")]
mod component_async {
    use super::*;
    use wasmtime::component::{Component, Linker, Resource, ResourceType};

    pub fn measure_execution_time(c: &mut Criterion) {
        let mut group = c.benchmark_group("component-async");
        host_to_wasm(&mut group);
        wasm_to_host(&mut group);
    }

    fn engine() -> Engine {
        let mut config = Config::new();
        config.wasm_component_model_async(true);
        Engine::new(&config).unwrap()
    }

    fn host_to_wasm(group: &mut BenchmarkGroup<'_, WallTime>) {
        let engine = engine();
        let component = Component::new(
            &engine,
            r#"
                (component
                    (core module $m
                        (import "" "task.return" (func $task-return))
                        (func (export "nop") (result i32)
                            call $task-return
                            i32.const 0
                        )
                        (func (export "callback") (param i32 i32 i32) (result i32)
                            unreachable
                        )
                    )
                    (core func $task-return (canon task.return))
                    (core instance $i (instantiate $m
                        (with "" (instance
                            (export "task.return" (func $task-return))
                        ))
                    ))
                    (func (export "nop") async
                        (canon lift (core func $i "nop")
                            async
                            (callback (core func $i "callback"))
                        )
                    )
                )
            "#,
        )
        .unwrap();
        let mut store = Store::new(&engine, ());
        let instance =
            run_await(Linker::new(&engine).instantiate_async(&mut store, &component)).unwrap();
        let nop = instance
            .get_typed_func::<(), ()>(&mut store, "nop")
            .unwrap();

        group.bench_function("host-to-wasm", |b| {
            b.iter(|| {
                run_await(store.run_concurrent(async |accessor| {
                    nop.call_concurrent(accessor, ()).await.unwrap()
                }))
                .unwrap();
            });
        });
    }

    fn wasm_to_host(group: &mut BenchmarkGroup<'_, WallTime>) {
        let mut config = Config::new();
        config.wasm_component_model_async(true);
        config.wasm_component_model_async_stackful(true);
        let engine = Engine::new(&config).unwrap();
        let component = Component::new(
            &engine,
            r#"
                (component
                    (import "r" (type $r (sub resource)))
                    (import "nop" (func $nop async))
                    (import "new" (func $new (result (own $r))))
                    (import "borrow" (func $borrow async (param "r" (borrow $r))))
                    (import "pending-once" (func $pending-once async))
                    (core module $memory (memory (export "memory") 1))
                    (core instance $memory (instantiate $memory))
                    (core func $nop (canon lower (func $nop) async))
                    (core func $new (canon lower (func $new)))
                    (core func $borrow (canon lower (func $borrow) async))
                    (core func $pending-once (canon lower (func $pending-once) async))
                    (core func $drop (canon resource.drop $r))
                    (core func $waitable-set-new (canon waitable-set.new))
                    (core func $waitable-join (canon waitable.join))
                    (core func $waitable-set-wait
                        (canon waitable-set.wait (memory (core memory $memory "memory"))))
                    (core func $waitable-set-drop (canon waitable-set.drop))
                    (core func $subtask-drop (canon subtask.drop))
                    (core module $m
                        (import "" "memory" (memory 1))
                        (import "" "nop" (func $nop (result i32)))
                        (import "" "new" (func $new (result i32)))
                        (import "" "borrow" (func $borrow (param i32) (result i32)))
                        (import "" "pending-once" (func $pending-once (result i32)))
                        (import "" "drop" (func $drop (param i32)))
                        (import "" "waitable-set.new" (func $waitable-set-new (result i32)))
                        (import "" "waitable.join" (func $waitable-join (param i32 i32)))
                        (import "" "waitable-set.wait"
                            (func $waitable-set-wait (param i32 i32) (result i32)))
                        (import "" "waitable-set.drop" (func $waitable-set-drop (param i32)))
                        (import "" "subtask.drop" (func $subtask-drop (param i32)))
                        (import "" "task.return" (func $task-return))
                        (func (export "run") (param $iters i64) (result i32)
                            loop $l
                                (drop (call $nop))
                                (local.tee $iters (i64.add (local.get $iters) (i64.const -1))) 
                                i64.const 0
                                i64.ne
                                br_if $l
                            end

                            call $task-return
                            i32.const 0
                        )
                        (func (export "run-borrow") (param $iters i64) (result i32)
                            (local $r i32)
                            (local.set $r (call $new))
                            loop $l
                                (drop (call $borrow (local.get $r)))
                                (local.tee $iters (i64.add (local.get $iters) (i64.const -1)))
                                i64.const 0
                                i64.ne
                                br_if $l
                            end

                            (call $drop (local.get $r))
                            call $task-return
                            i32.const 0
                        )
                        (func (export "run-pending-once") (param $iters i64)
                            (local $ret i32)
                            (local $task i32)
                            (local $set i32)
                            (local.set $set (call $waitable-set-new))
                            loop $l
                                (local.set $ret (call $pending-once))
                                ;; Verify that the first poll returned Pending.
                                (if (i32.ne
                                        (i32.and (local.get $ret) (i32.const 0xf))
                                        (i32.const 1))
                                    (then unreachable))
                                (local.set $task
                                    (i32.shr_u (local.get $ret) (i32.const 4)))
                                (call $waitable-join
                                    (local.get $task) (local.get $set))
                                (drop (call $waitable-set-wait
                                    (local.get $set) (i32.const 0)))
                                (call $subtask-drop (local.get $task))
                                (local.tee $iters
                                    (i64.sub (local.get $iters) (i64.const 1)))
                                i64.const 0
                                i64.ne
                                br_if $l
                            end
                            (call $waitable-set-drop (local.get $set))
                            call $task-return
                        )
                        (func (export "callback") (param i32 i32 i32) (result i32)
                            unreachable
                        )
                    )
                    (core func $task-return (canon task.return))
                    (core instance $i (instantiate $m
                        (with "" (instance
                            (export "memory" (memory $memory "memory"))
                            (export "nop" (func $nop))
                            (export "new" (func $new))
                            (export "borrow" (func $borrow))
                            (export "pending-once" (func $pending-once))
                            (export "drop" (func $drop))
                            (export "waitable-set.new" (func $waitable-set-new))
                            (export "waitable.join" (func $waitable-join))
                            (export "waitable-set.wait" (func $waitable-set-wait))
                            (export "waitable-set.drop" (func $waitable-set-drop))
                            (export "subtask.drop" (func $subtask-drop))
                            (export "task.return" (func $task-return))
                        ))
                    ))
                    (func (export "run") async (param "iterations" u64)
                        (canon lift (core func $i "run")
                            async
                            (callback (core func $i "callback"))
                        )
                    )
                    (func (export "run-borrow") async (param "iterations" u64)
                        (canon lift (core func $i "run-borrow")
                            async
                            (callback (core func $i "callback"))
                        )
                    )
                    (func (export "run-pending-once") async (param "iterations" u64)
                        (canon lift (core func $i "run-pending-once") async)
                    )
                )
            "#,
        )
        .unwrap();
        let mut store = Store::new(&engine, ());
        let mut linker = Linker::new(&engine);
        linker
            .root()
            .resource("r", ResourceType::host::<u32>(), |_, _| Ok(()))
            .unwrap();
        linker
            .root()
            .func_wrap_concurrent("nop", |_, ()| Box::pin(async { Ok(()) }))
            .unwrap();
        linker
            .root()
            .func_wrap("new", |_, ()| Ok((Resource::<u32>::new_own(0),)))
            .unwrap();
        linker
            .root()
            .func_wrap_concurrent("borrow", |_, (_r,): (Resource<u32>,)| {
                Box::pin(async { Ok(()) })
            })
            .unwrap();
        linker
            .root()
            .func_wrap_concurrent("pending-once", |_, ()| {
                let mut pending = true;
                Box::pin(std::future::poll_fn(move |cx| {
                    if std::mem::take(&mut pending) {
                        cx.waker().wake_by_ref();
                        Poll::Pending
                    } else {
                        Poll::Ready(Ok(()))
                    }
                }))
            })
            .unwrap();
        let instance = run_await(linker.instantiate_async(&mut store, &component)).unwrap();
        let run = instance
            .get_typed_func::<(u64,), ()>(&mut store, "run")
            .unwrap();

        group.bench_function("wasm-to-host", |b| {
            b.iter_custom(|iterations| {
                let start = Instant::now();
                run_await(store.run_concurrent(async |accessor| {
                    run.call_concurrent(accessor, (iterations,)).await.unwrap()
                }))
                .unwrap();
                start.elapsed()
            });
        });

        let run_borrow = instance
            .get_typed_func::<(u64,), ()>(&mut store, "run-borrow")
            .unwrap();
        group.bench_function("wasm-to-host-borrow", |b| {
            b.iter_custom(|iterations| {
                let start = Instant::now();
                run_await(store.run_concurrent(async |accessor| {
                    run_borrow
                        .call_concurrent(accessor, (iterations,))
                        .await
                        .unwrap()
                }))
                .unwrap();
                start.elapsed()
            });
        });

        let run_pending_once = instance
            .get_typed_func::<(u64,), ()>(&mut store, "run-pending-once")
            .unwrap();

        group.bench_function("wasm-to-host-pending-once", |b| {
            b.iter_custom(|iterations| {
                let start = Instant::now();
                run_await(store.run_concurrent(async |accessor| {
                    run_pending_once
                        .call_concurrent(accessor, (iterations,))
                        .await
                        .unwrap()
                }))
                .unwrap();
                start.elapsed()
            });
        });
    }
}

#[cfg(feature = "component-model-async")]
mod component_streams {
    use super::*;
    use futures::channel::oneshot;
    use wasmtime::component::{
        Component, ComponentType, Instance, Lift, Linker, Lower, Source, StreamAny, StreamConsumer,
        StreamReader, StreamResult, Type, TypedFunc, Val,
    };

    /// Items per stream in each iteration.
    const ITEMS: u32 = 1024;

    pub fn measure_execution_time(c: &mut Criterion) {
        let mut group = c.benchmark_group("component-streams");
        group.throughput(criterion::Throughput::Elements(ITEMS.into()));
        let engine = engine();
        typed::<u32>(&mut group, &engine, "u32", "", 0);
        typed::<String>(
            &mut group,
            &engine,
            "string",
            STRING_DATA,
            "hello".to_string(),
        );
        vals(&mut group, &engine, "u32", "", Val::U32(0));
        vals(
            &mut group,
            &engine,
            "string",
            STRING_DATA,
            Val::String("hello".into()),
        );
    }

    fn engine() -> Engine {
        let mut config = Config::new();
        config.wasm_component_model_async(true);
        Engine::new(&config).unwrap()
    }

    /// Fills the 64 items at address 1024 that `fill` writes with `(ptr, len)`
    /// pairs pointing at "hello".
    const STRING_DATA: &str = r#"
        (data (i32.const 512) "hello")
        (func $init
          (local $i i32)
          (loop $l
            (i32.store (i32.add (i32.const 1024) (i32.shl (local.get $i) (i32.const 3)))
              (i32.const 512))
            (i32.store (i32.add (i32.const 1028) (i32.shl (local.get $i) (i32.const 3)))
              (i32.const 5))
            (local.set $i (i32.add (local.get $i) (i32.const 1)))
            (br_if $l (i32.lt_u (local.get $i) (i32.const 64)))))
        (start $init)
    "#;

    /// Returns a component for `stream<{payload}>` whose async `drain` export
    /// reads a stream until it is dropped, and whose async `fill` export
    /// returns a new stream to which it writes `n` items copied from the 64
    /// at address 1024, initialized by `data`.
    fn component(engine: &Engine, payload: &str, data: &str) -> Component {
        let wat = format!(
            r#"
(component
  (type $s (stream {payload}))
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
      (local.get $ret))
    (func (export "reset") (global.set $heap (i32.const 4096))))
  (core instance $libc (instantiate $libc))
  (core func $stream.new (canon stream.new $s))
  (core func $stream.read
    (canon stream.read $s async (memory (core memory $libc "mem"))
      (realloc (core func $libc "realloc"))))
  (core func $stream.write
    (canon stream.write $s async (memory (core memory $libc "mem"))))
  (core func $stream.drop-readable (canon stream.drop-readable $s))
  (core func $stream.drop-writable (canon stream.drop-writable $s))
  (core func $drain.return (canon task.return))
  (core func $fill.return (canon task.return (result $s)))
  (core func $waitable-set.new (canon waitable-set.new))
  (core func $waitable.join (canon waitable.join))
  (core module $m
    (import "" "mem" (memory 1))
    (import "" "reset" (func $reset))
    (import "" "stream.new" (func $stream.new (result i64)))
    (import "" "stream.read" (func $stream.read (param i32 i32 i32) (result i32)))
    (import "" "stream.write" (func $stream.write (param i32 i32 i32) (result i32)))
    (import "" "stream.drop-readable" (func $stream.drop-readable (param i32)))
    (import "" "stream.drop-writable" (func $stream.drop-writable (param i32)))
    (import "" "drain.return" (func $drain.return))
    (import "" "fill.return" (func $fill.return (param i32)))
    (import "" "waitable-set.new" (func $waitable-set.new (result i32)))
    (import "" "waitable.join" (func $waitable.join (param i32 i32)))

    (global $in (mut i32) (i32.const 0))
    (global $out (mut i32) (i32.const 0))
    (global $left (mut i32) (i32.const 0))
    (global $ws (mut i32) (i32.const 0))
    {data}

    ;; Waits for `$handle`, returning CALLBACK_CODE_WAIT for `$ws`.
    (func $wait (param $handle i32) (result i32)
      (call $waitable.join (local.get $handle) (global.get $ws))
      (i32.or (i32.const 2) (i32.shl (global.get $ws) (i32.const 4))))

    (func $read (result i32)
      (local $code i32)
      (call $reset)
      (local.set $code
        (call $stream.read (global.get $in) (i32.const 1024) (i32.const 64)))
      (if (i32.eq (local.get $code) (i32.const -1))
        (then (return (call $wait (global.get $in)))))
      (call $on-read (local.get $code)))

    (func $on-read (param $code i32) (result i32)
      (if (i32.eqz (i32.and (local.get $code) (i32.const 0xf)))
        (then (return (call $read))))
      (call $stream.drop-readable (global.get $in))
      (call $drain.return)
      (i32.const 0)) ;; CALLBACK_CODE_EXIT

    (func (export "drain") (param $in i32) (result i32)
      (global.set $in (local.get $in))
      (if (i32.eqz (global.get $ws))
        (then (global.set $ws (call $waitable-set.new))))
      (call $read))

    (func $write (result i32)
      (local $code i32)
      (if (i32.eqz (global.get $left))
        (then
          (call $stream.drop-writable (global.get $out))
          (return (i32.const 0)))) ;; CALLBACK_CODE_EXIT
      (local.set $code
        (call $stream.write (global.get $out) (i32.const 1024)
          (select (global.get $left) (i32.const 64)
            (i32.lt_u (global.get $left) (i32.const 64)))))
      (if (i32.eq (local.get $code) (i32.const -1))
        (then (return (call $wait (global.get $out)))))
      (call $on-write (local.get $code)))

    (func $on-write (param $code i32) (result i32)
      ;; The host consumer takes everything, so anything else is a bug here.
      (if (i32.ne (i32.and (local.get $code) (i32.const 0xf)) (i32.const 0))
        (then unreachable))
      (global.set $left
        (i32.sub (global.get $left) (i32.shr_u (local.get $code) (i32.const 4))))
      (call $write))

    (func (export "fill") (param $n i32) (result i32)
      (local $rw i64)
      (global.set $left (local.get $n))
      (local.set $rw (call $stream.new))
      (global.set $out (i32.wrap_i64 (i64.shr_u (local.get $rw) (i64.const 32))))
      (if (i32.eqz (global.get $ws))
        (then (global.set $ws (call $waitable-set.new))))
      (call $fill.return (i32.wrap_i64 (local.get $rw)))
      (call $write))

    (func (export "cb") (param $event i32) (param $handle i32) (param $code i32) (result i32)
      (call $waitable.join (local.get $handle) (i32.const 0))
      (if (i32.eq (local.get $handle) (global.get $in))
        (then (return (call $on-read (local.get $code)))))
      (call $on-write (local.get $code))))
  (core instance $i (instantiate $m (with "" (instance
    (export "mem" (memory $libc "mem"))
    (export "reset" (func $libc "reset"))
    (export "stream.new" (func $stream.new))
    (export "stream.read" (func $stream.read))
    (export "stream.write" (func $stream.write))
    (export "stream.drop-readable" (func $stream.drop-readable))
    (export "stream.drop-writable" (func $stream.drop-writable))
    (export "drain.return" (func $drain.return))
    (export "fill.return" (func $fill.return))
    (export "waitable-set.new" (func $waitable-set.new))
    (export "waitable.join" (func $waitable.join))))))
  (func (export "drain") async (param "s" $s)
    (canon lift (core func $i "drain") async (callback (core func $i "cb"))))
  (func (export "fill") async (param "n" u32) (result $s)
    (canon lift (core func $i "fill") async (callback (core func $i "cb"))))
)
"#
        );
        Component::new(engine, wat).unwrap()
    }

    fn instantiate(engine: &Engine, component: &Component) -> (Store<()>, Instance) {
        let mut store = Store::new(engine, ());
        let instance =
            run_await(Linker::new(engine).instantiate_async(&mut store, component)).unwrap();
        (store, instance)
    }

    /// A consumer which reads and drops every item, using `read`, and which
    /// signals `done` when dropped at the end of the stream.
    struct Discard<T> {
        read: fn(&mut Source<'_, T>, StoreContextMut<'_, ()>, &mut Vec<T>) -> Result<()>,
        buffer: Vec<T>,
        done: Option<oneshot::Sender<()>>,
    }

    impl<T> Discard<T> {
        fn new(
            read: fn(&mut Source<'_, T>, StoreContextMut<'_, ()>, &mut Vec<T>) -> Result<()>,
        ) -> (Self, oneshot::Receiver<()>) {
            let (tx, rx) = oneshot::channel();
            let consumer = Discard {
                read,
                buffer: Vec::with_capacity(64),
                done: Some(tx),
            };
            (consumer, rx)
        }
    }

    impl<T> Drop for Discard<T> {
        fn drop(&mut self) {
            let _ = self.done.take().unwrap().send(());
        }
    }

    impl<T: Send + 'static> StreamConsumer<()> for Discard<T> {
        type Item = T;

        fn poll_consume(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            store: StoreContextMut<()>,
            mut src: Source<'_, T>,
            _finish: bool,
        ) -> Poll<Result<StreamResult>> {
            let this = &mut *self;
            this.buffer.clear();
            (this.read)(&mut src, store, &mut this.buffer)?;
            Poll::Ready(Ok(StreamResult::Completed))
        }
    }

    impl<T> Unpin for Discard<T> {}

    fn read_typed<T: Lift + 'static>(
        src: &mut Source<'_, T>,
        store: StoreContextMut<'_, ()>,
        buffer: &mut Vec<T>,
    ) -> Result<()> {
        src.read(store, buffer)
    }

    /// Benchmarks host-to-wasm and wasm-to-host streams of statically typed
    /// `item`s.
    fn typed<T>(
        group: &mut BenchmarkGroup<'_, WallTime>,
        engine: &Engine,
        name: &str,
        data: &str,
        item: T,
    ) where
        T: ComponentType + Lower + Lift + Clone + Send + Sync + Unpin + 'static,
    {
        let component = component(engine, name, data);

        let (mut store, instance) = instantiate(engine, &component);
        let drain: TypedFunc<(StreamReader<T>,), ()> =
            instance.get_typed_func(&mut store, "drain").unwrap();
        group.bench_function(&format!("host-to-wasm - typed - {name}"), |b| {
            b.iter(|| {
                let items = vec![item.clone(); ITEMS as usize];
                let reader = StreamReader::new(&mut store, items).unwrap();
                run_await(drain.call_async(&mut store, (reader,))).unwrap();
            });
        });

        let (mut store, instance) = instantiate(engine, &component);
        let fill: TypedFunc<(u32,), (StreamReader<T>,)> =
            instance.get_typed_func(&mut store, "fill").unwrap();
        group.bench_function(&format!("wasm-to-host - typed - {name}"), |b| {
            b.iter(|| {
                let (reader,) = run_await(fill.call_async(&mut store, (ITEMS,))).unwrap();
                let (consumer, done) = Discard::new(read_typed::<T>);
                reader.pipe(&mut store, consumer).unwrap();
                run_await(store.run_concurrent(async |_| done.await))
                    .unwrap()
                    .unwrap();
            });
        });
    }

    fn read_vals(
        src: &mut Source<'_, Val>,
        store: StoreContextMut<'_, ()>,
        buffer: &mut Vec<Val>,
    ) -> Result<()> {
        src.read_vals(store, buffer)
    }

    /// Benchmarks host-to-wasm and wasm-to-host streams of `item`s as `Val`s,
    /// for comparison with `typed`.
    fn vals(
        group: &mut BenchmarkGroup<'_, WallTime>,
        engine: &Engine,
        name: &str,
        data: &str,
        item: Val,
    ) {
        let component = component(engine, name, data);

        let (mut store, instance) = instantiate(engine, &component);
        let func = instance.get_func(&mut store, "drain").unwrap();
        let Type::Stream(ty) = func.ty(&store).params().next().unwrap().1 else {
            unreachable!()
        };
        let drain = func.typed::<(StreamAny,), ()>(&store).unwrap();
        group.bench_function(&format!("host-to-wasm - val - {name}"), |b| {
            b.iter(|| {
                let items = vec![item.clone(); ITEMS as usize];
                let stream = StreamAny::new(&mut store, ty.clone(), items).unwrap();
                run_await(drain.call_async(&mut store, (stream,))).unwrap();
            });
        });

        let (mut store, instance) = instantiate(engine, &component);
        let fill: TypedFunc<(u32,), (StreamAny,)> =
            instance.get_typed_func(&mut store, "fill").unwrap();
        group.bench_function(&format!("wasm-to-host - val - {name}"), |b| {
            b.iter(|| {
                let (stream,) = run_await(fill.call_async(&mut store, (ITEMS,))).unwrap();
                let (consumer, done) = Discard::new(read_vals);
                stream.pipe(&mut store, consumer).unwrap();
                run_await(store.run_concurrent(async |_| done.await))
                    .unwrap()
                    .unwrap();
            });
        });
    }
}

mod indirect {
    use super::*;
    use std::time::Duration;

    pub fn measure_execution_time(c: &mut Criterion) {
        let _ = env_logger::try_init();
        let mut group = c.benchmark_group("call-indirect");
        for lazy in [true, false] {
            // Note: the seemingly useless loop over a single `calls` value is
            // just there to make it easy to play around with different numbers
            // of calls.
            for calls in [65536] {
                group.throughput(criterion::Throughput::Elements(calls));
                same_callee(&mut group, lazy, calls);
                different_callees(&mut group, lazy, calls);
            }
        }
    }

    fn same_callee(group: &mut BenchmarkGroup<'_, WallTime>, lazy: bool, calls: u64) {
        let name = format!(
            "same-callee/table-init-{}/{calls}-calls",
            if lazy { "lazy" } else { "strict" }
        );
        group.bench_function(name, |b| {
            let mut config = Config::new();
            config.table_lazy_init(lazy);
            let engine = Engine::new(&config).unwrap();

            let table_module = Module::new(
                &engine,
                r#"
                    (module
                        (func)
                        (table (export "table") 5 5 funcref)
                        (elem (table 0) (i32.const 0) func 0 0 0 0 0)
                    )
                "#,
            )
            .unwrap();

            let run_module = Module::new(
                &engine,
                r#"
                    (module
                        (type $ty (func))
                        (import "" "table" (table 0 funcref))
                        (func (export "run") (param $callee i32) (param $calls i32)
                            loop
                                (if (i32.eqz (local.get $calls))
                                    (then (return)))
                                (local.set $calls (i32.sub (local.get $calls) (i32.const 1)))
                                (call_indirect (type $ty) (local.get $callee))
                                br 0
                            end
                        )
                    )
                "#,
            )
            .unwrap();

            b.iter_custom(move |iters| {
                let mut total = Duration::from_millis(0);

                for _ in 0..iters {
                    let mut store = Store::new(&engine, ());

                    let table_instance = Instance::new(&mut store, &table_module, &[]).unwrap();
                    let table = table_instance.get_table(&mut store, "table").unwrap();

                    let run_instance =
                        Instance::new(&mut store, &run_module, &[table.into()]).unwrap();
                    let run = run_instance
                        .get_typed_func::<(u32, u32), ()>(&mut store, "run")
                        .unwrap();

                    let start = Instant::now();
                    let result = run.call(&mut store, (0, calls.try_into().unwrap()));
                    total += start.elapsed();

                    result.unwrap();
                }

                total
            });
        });
    }

    fn different_callees(group: &mut BenchmarkGroup<'_, WallTime>, lazy: bool, calls: u64) {
        let name = format!(
            "different-callees/table-init-{}/{calls}-calls",
            if lazy { "lazy" } else { "strict" }
        );
        group.bench_function(name, |b| {
            let mut config = Config::new();
            config.table_lazy_init(lazy);
            let engine = Engine::new(&config).unwrap();

            let mut table_wat = format!(
                "
                    (module
                        (func)
                        (table (export \"table\") {calls} {calls} funcref)
                        (elem (table 0) (i32.const 0) func"
            );
            for _ in 0..calls {
                table_wat.push_str(" 0");
            }
            table_wat.push_str("))");
            let table_module = Module::new(&engine, &table_wat).unwrap();

            let run_module = Module::new(
                &engine,
                r#"
                    (module
                        (type $ty (func))
                        (import "" "table" (table 0 funcref))
                        (func (export "run") (param $callee i32) (param $calls i32)
                            loop
                                (if (i32.eqz (local.get $calls))
                                    (then (return)))
                                (local.set $calls (i32.sub (local.get $calls) (i32.const 1)))

                                (call_indirect (type $ty) (local.get $callee))
                                (local.set $callee (i32.add (local.get $callee) (i32.const 1)))

                                br 0
                            end
                        )
                    )
                "#,
            )
            .unwrap();

            b.iter_custom(move |iters| {
                let mut total = Duration::from_millis(0);

                for _ in 0..iters {
                    let mut store = Store::new(&engine, ());

                    let table_instance = Instance::new(&mut store, &table_module, &[]).unwrap();
                    let table = table_instance.get_table(&mut store, "table").unwrap();

                    let run_instance =
                        Instance::new(&mut store, &run_module, &[table.into()]).unwrap();
                    let run = run_instance
                        .get_typed_func::<(u32, u32), ()>(&mut store, "run")
                        .unwrap();

                    let start = Instant::now();
                    let result = run.call(&mut store, (0, calls.try_into().unwrap()));
                    total += start.elapsed();

                    result.unwrap();
                }

                total
            });
        });
    }
}
