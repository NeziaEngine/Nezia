//! Command (ring buffer) vs Shared memory (ArcSwap) のパフォーマンス比較ベンチ。
//!
//! 毎フレームの「リスナー更新 + 全ソース位置更新」を 3 つの経路で実装し、
//! 1 フレームあたりのコストを比較する。
//!
//! 実行: `cargo run --example bench_commands --release`

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::UnsafeCell;
use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::Instant;

use arc_swap::ArcSwap;

// =========================================================================
// Counting allocator: alloc 回数とバイト数を計測する。
// =========================================================================

static ALLOC_COUNT: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);
static DEALLOC_COUNT: AtomicU64 = AtomicU64::new(0);
static DEALLOC_BYTES: AtomicU64 = AtomicU64::new(0);

struct CountingAlloc;

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        ALLOC_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        DEALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        DEALLOC_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

#[derive(Clone, Copy, Default)]
struct AllocSnapshot {
    allocs: u64,
    bytes: u64,
    deallocs: u64,
    dealloc_bytes: u64,
}

impl AllocSnapshot {
    fn now() -> Self {
        Self {
            allocs: ALLOC_COUNT.load(Ordering::Relaxed),
            bytes: ALLOC_BYTES.load(Ordering::Relaxed),
            deallocs: DEALLOC_COUNT.load(Ordering::Relaxed),
            dealloc_bytes: DEALLOC_BYTES.load(Ordering::Relaxed),
        }
    }
    fn diff(self, before: Self) -> AllocDelta {
        AllocDelta {
            allocs: self.allocs - before.allocs,
            bytes: self.bytes - before.bytes,
            deallocs: self.deallocs - before.deallocs,
            dealloc_bytes: self.dealloc_bytes - before.dealloc_bytes,
        }
    }
}

#[derive(Clone, Copy, Default)]
#[allow(dead_code)]
struct AllocDelta {
    allocs: u64,
    bytes: u64,
    deallocs: u64,
    dealloc_bytes: u64,
}
use ringbuf::{
    HeapRb,
    traits::{Consumer, Producer, Split},
};

const SPATIAL_BATCH_SIZE: usize = 32;

#[derive(Clone, Copy)]
#[allow(dead_code)]
struct EntityId {
    index: u32,
    generation: u32,
}

#[derive(Clone, Copy, Default)]
#[allow(dead_code)]
struct ListenerState {
    position: [f32; 3],
    forward: [f32; 3],
    up: [f32; 3],
    right: [f32; 3],
}

// =========================================================================
// Path A: Command ring buffer (現状の設計)
// =========================================================================

#[derive(Clone, Copy)]
#[allow(clippy::large_enum_variant)]
enum Command {
    SetListener {
        position: [f32; 3],
        forward: [f32; 3],
        up: [f32; 3],
    },
    BatchSetSourcePositions {
        count: u8,
        updates: [(EntityId, [f32; 3]); SPATIAL_BATCH_SIZE],
    },
}

struct WorldA {
    positions_x: Vec<f32>,
    positions_y: Vec<f32>,
    positions_z: Vec<f32>,
    listener: ListenerState,
    sparse: Vec<Option<u32>>,
}

impl WorldA {
    fn new(n: usize) -> Self {
        Self {
            positions_x: vec![0.0; n],
            positions_y: vec![0.0; n],
            positions_z: vec![0.0; n],
            listener: ListenerState::default(),
            sparse: (0..n as u32).map(Some).collect(),
        }
    }

    fn resolve(&self, id: EntityId) -> Option<usize> {
        self.sparse
            .get(id.index as usize)
            .and_then(|x| x.map(|d| d as usize))
    }
}

fn apply_cmd(world: &mut WorldA, cmd: Command) {
    match cmd {
        Command::SetListener {
            position,
            forward,
            up,
        } => {
            world.listener.position = position;
            world.listener.forward = forward;
            world.listener.up = up;
            // right は通常正規化計算するが、ベンチでは省略
        }
        Command::BatchSetSourcePositions { count, updates } => {
            for (id, pos) in &updates[..count as usize] {
                if let Some(d) = world.resolve(*id) {
                    world.positions_x[d] = pos[0];
                    world.positions_y[d] = pos[1];
                    world.positions_z[d] = pos[2];
                }
            }
        }
    }
}

fn bench_command(n: usize, frames: usize) -> BenchResult {
    let setup_before = AllocSnapshot::now();
    // 1 frame で必要なスロット数: ceil(n / 32) + 1 (listener)
    let cap = (n / SPATIAL_BATCH_SIZE + 2).max(16) * 2;
    let ring = HeapRb::<Command>::new(cap);
    let (mut prod, mut cons) = ring.split();
    let mut world = WorldA::new(n);

    let positions: Vec<[f32; 3]> = (0..n)
        .map(|i| [i as f32, i as f32 * 0.5, i as f32 * 0.25])
        .collect();
    let ids: Vec<EntityId> = (0..n as u32)
        .map(|i| EntityId {
            index: i,
            generation: 0,
        })
        .collect();
    let setup_after = AllocSnapshot::now();

    let mut main_total = 0u128;
    let mut sound_total = 0u128;
    let dummy = (
        EntityId {
            index: 0,
            generation: 0,
        },
        [0.0f32; 3],
    );

    for _ in 0..frames {
        // main: produce
        let t = Instant::now();
        let _ = prod.try_push(Command::SetListener {
            position: [1.0, 2.0, 3.0],
            forward: [0.0, 0.0, -1.0],
            up: [0.0, 1.0, 0.0],
        });
        let mut buf = [dummy; SPATIAL_BATCH_SIZE];
        let mut i = 0;
        while i < n {
            let end = (i + SPATIAL_BATCH_SIZE).min(n);
            let count = (end - i) as u8;
            for k in 0..(end - i) {
                buf[k] = (ids[i + k], positions[i + k]);
            }
            let _ = prod.try_push(Command::BatchSetSourcePositions {
                count,
                updates: buf,
            });
            i = end;
        }
        main_total += t.elapsed().as_nanos();

        // sound: drain
        let t = Instant::now();
        while let Some(cmd) = cons.try_pop() {
            apply_cmd(&mut world, cmd);
        }
        sound_total += t.elapsed().as_nanos();
    }
    let frame_after = AllocSnapshot::now();

    black_box(&world);
    BenchResult {
        main_ns: main_total,
        sound_ns: sound_total,
        setup_alloc: setup_after.diff(setup_before),
        frame_alloc: frame_after.diff(setup_after),
        static_bytes: cap * std::mem::size_of::<Command>(),
    }
}

// =========================================================================
// Path B: ArcSwap (毎フレーム Vec を確保して store)
// =========================================================================

struct SharedB {
    listener: ArcSwap<ListenerState>,
    positions: ArcSwap<Vec<[f32; 3]>>,
}

fn bench_arcswap(n: usize, frames: usize) -> BenchResult {
    let setup_before = AllocSnapshot::now();
    let shared = Arc::new(SharedB {
        listener: ArcSwap::from_pointee(ListenerState::default()),
        positions: ArcSwap::from_pointee(vec![[0.0f32; 3]; n]),
    });
    let mut dense_x = vec![0.0f32; n];
    let mut dense_y = vec![0.0f32; n];
    let mut dense_z = vec![0.0f32; n];
    let mut local_listener = ListenerState::default();

    let positions: Vec<[f32; 3]> = (0..n)
        .map(|i| [i as f32, i as f32 * 0.5, i as f32 * 0.25])
        .collect();
    let setup_after = AllocSnapshot::now();

    let mut main_total = 0u128;
    let mut sound_total = 0u128;

    for _ in 0..frames {
        // main: 新しい Arc を作って swap
        let t = Instant::now();
        shared.listener.store(Arc::new(ListenerState {
            position: [1.0, 2.0, 3.0],
            forward: [0.0, 0.0, -1.0],
            up: [0.0, 1.0, 0.0],
            right: [1.0, 0.0, 0.0],
        }));
        let mut snap = Vec::with_capacity(n);
        snap.extend_from_slice(&positions);
        shared.positions.store(Arc::new(snap));
        main_total += t.elapsed().as_nanos();

        // sound: load + dense SoA に展開
        let t = Instant::now();
        let l = shared.listener.load();
        local_listener = **l;
        let p = shared.positions.load();
        for (i, v) in p.iter().enumerate() {
            dense_x[i] = v[0];
            dense_y[i] = v[1];
            dense_z[i] = v[2];
        }
        sound_total += t.elapsed().as_nanos();
    }
    let frame_after = AllocSnapshot::now();

    black_box(&dense_x);
    black_box(&local_listener);
    BenchResult {
        main_ns: main_total,
        sound_ns: sound_total,
        setup_alloc: setup_after.diff(setup_before),
        frame_alloc: frame_after.diff(setup_after),
        static_bytes: std::mem::size_of::<ListenerState>() + n * std::mem::size_of::<[f32; 3]>(),
    }
}

// =========================================================================
// Path C: 同期コストゼロの下限参照値
// データ動線は D (triple buffer) と同じ:
//   main : AoS positions → AoS shared (memcpy)
//   sound: AoS shared → SoA dense (要素ごと展開)
// 違うのは「同期機構が一切ない」点のみ。D との差分が純粋な triple buffer
// オーバーヘッドに対応する。
// =========================================================================

fn bench_direct(n: usize, frames: usize) -> BenchResult {
    let setup_before = AllocSnapshot::now();
    let mut shared_positions = vec![[0.0f32; 3]; n];
    let mut shared_listener;
    let mut dense_x = vec![0.0f32; n];
    let mut dense_y = vec![0.0f32; n];
    let mut dense_z = vec![0.0f32; n];
    let mut local_listener = ListenerState::default();

    let positions: Vec<[f32; 3]> = (0..n)
        .map(|i| [i as f32, i as f32 * 0.5, i as f32 * 0.25])
        .collect();
    let setup_after = AllocSnapshot::now();

    let mut main_total = 0u128;
    let mut sound_total = 0u128;

    for _ in 0..frames {
        // main: AoS → AoS memcpy
        let t = Instant::now();
        shared_listener = ListenerState {
            position: [1.0, 2.0, 3.0],
            forward: [0.0, 0.0, -1.0],
            up: [0.0, 1.0, 0.0],
            right: [1.0, 0.0, 0.0],
        };
        shared_positions.copy_from_slice(&positions);
        main_total += t.elapsed().as_nanos();

        // sound: AoS → SoA 展開
        let t = Instant::now();
        local_listener = shared_listener;
        for (i, v) in shared_positions.iter().enumerate() {
            dense_x[i] = v[0];
            dense_y[i] = v[1];
            dense_z[i] = v[2];
        }
        sound_total += t.elapsed().as_nanos();
    }
    let frame_after = AllocSnapshot::now();

    black_box(&dense_x);
    black_box(&local_listener);
    BenchResult {
        main_ns: main_total,
        sound_ns: sound_total,
        setup_alloc: setup_after.diff(setup_before),
        frame_alloc: frame_after.diff(setup_after),
        static_bytes: std::mem::size_of::<ListenerState>() + n * std::mem::size_of::<[f32; 3]>(),
    }
}

// =========================================================================
// Path D: Triple buffer (alloc 無し、SPSC, lock-free)
// 3 スロットを事前確保し、書き込み側は back スロットを書き換え、publish で
// back ↔ ready をスワップ、読み取り側は front ↔ ready をスワップ。
// =========================================================================

const BACK_MASK: u8 = 0b00_00_00_11;
const READY_MASK: u8 = 0b00_00_11_00;
const FRONT_MASK: u8 = 0b00_11_00_00;
const DIRTY_BIT: u8 = 0b01_00_00_00;

struct TripleBuf<T> {
    slots: [UnsafeCell<T>; 3],
    state: AtomicU8,
}

unsafe impl<T: Send> Send for TripleBuf<T> {}
unsafe impl<T: Send> Sync for TripleBuf<T> {}

impl<T> TripleBuf<T> {
    fn new(a: T, b: T, c: T) -> Self {
        // 初期: back=0, ready=1, front=2, dirty=0
        Self {
            slots: [UnsafeCell::new(a), UnsafeCell::new(b), UnsafeCell::new(c)],
            state: AtomicU8::new((1 << 2) | (2 << 4)),
        }
    }

    /// SAFETY: writer thread からのみ呼ぶこと（interior mutability via UnsafeCell）。
    #[allow(clippy::mut_from_ref)]
    unsafe fn back_mut(&self) -> &mut T {
        let s = self.state.load(Ordering::Acquire);
        let back = (s & BACK_MASK) as usize;
        unsafe { &mut *self.slots[back].get() }
    }

    fn publish(&self) {
        loop {
            let s = self.state.load(Ordering::Acquire);
            let back = s & BACK_MASK;
            let ready = (s & READY_MASK) >> 2;
            let front = s & FRONT_MASK;
            let new = ready | (back << 2) | front | DIRTY_BIT;
            if self
                .state
                .compare_exchange_weak(s, new, Ordering::Release, Ordering::Relaxed)
                .is_ok()
            {
                return;
            }
        }
    }

    /// SAFETY: reader thread からのみ呼ぶこと。
    unsafe fn read(&self) -> &T {
        loop {
            let s = self.state.load(Ordering::Acquire);
            if s & DIRTY_BIT == 0 {
                let front = ((s & FRONT_MASK) >> 4) as usize;
                return unsafe { &*self.slots[front].get() };
            }
            let back = s & BACK_MASK;
            let ready = (s & READY_MASK) >> 2;
            let front = (s & FRONT_MASK) >> 4;
            let new = back | (front << 2) | (ready << 4);
            if self
                .state
                .compare_exchange_weak(s, new, Ordering::Release, Ordering::Relaxed)
                .is_ok()
            {
                return unsafe { &*self.slots[ready as usize].get() };
            }
        }
    }
}

fn bench_triple(n: usize, frames: usize) -> BenchResult {
    let setup_before = AllocSnapshot::now();
    let listener_buf = Arc::new(TripleBuf::new(
        ListenerState::default(),
        ListenerState::default(),
        ListenerState::default(),
    ));
    let positions_buf = Arc::new(TripleBuf::new(
        vec![[0.0f32; 3]; n],
        vec![[0.0f32; 3]; n],
        vec![[0.0f32; 3]; n],
    ));

    let mut dense_x = vec![0.0f32; n];
    let mut dense_y = vec![0.0f32; n];
    let mut dense_z = vec![0.0f32; n];
    let mut local_listener = ListenerState::default();

    let positions: Vec<[f32; 3]> = (0..n)
        .map(|i| [i as f32, i as f32 * 0.5, i as f32 * 0.25])
        .collect();
    let setup_after = AllocSnapshot::now();

    let mut main_total = 0u128;
    let mut sound_total = 0u128;

    for _ in 0..frames {
        // main: back スロットへ直接書き込み → publish
        let t = Instant::now();
        unsafe {
            let l = listener_buf.back_mut();
            *l = ListenerState {
                position: [1.0, 2.0, 3.0],
                forward: [0.0, 0.0, -1.0],
                up: [0.0, 1.0, 0.0],
                right: [1.0, 0.0, 0.0],
            };
        }
        listener_buf.publish();
        unsafe {
            let p = positions_buf.back_mut();
            // back スロットの容量は事前確保済み。memcpy で上書き。
            p.copy_from_slice(&positions);
        }
        positions_buf.publish();
        main_total += t.elapsed().as_nanos();

        // sound: front を読んで dense SoA に展開
        let t = Instant::now();
        let l = unsafe { listener_buf.read() };
        local_listener = *l;
        let p = unsafe { positions_buf.read() };
        for (i, v) in p.iter().enumerate() {
            dense_x[i] = v[0];
            dense_y[i] = v[1];
            dense_z[i] = v[2];
        }
        sound_total += t.elapsed().as_nanos();
    }
    let frame_after = AllocSnapshot::now();

    black_box(&dense_x);
    black_box(&local_listener);
    BenchResult {
        main_ns: main_total,
        sound_ns: sound_total,
        setup_alloc: setup_after.diff(setup_before),
        frame_alloc: frame_after.diff(setup_after),
        static_bytes: std::mem::size_of::<ListenerState>() * 3
            + 3 * n * std::mem::size_of::<[f32; 3]>(),
    }
}

// =========================================================================
// Path E: triple_buffer クレート（unsafe を自前で書かない版）
// =========================================================================

fn bench_triple_crate(n: usize, frames: usize) -> BenchResult {
    let setup_before = AllocSnapshot::now();
    let (mut listener_in, mut listener_out) =
        triple_buffer::triple_buffer(&ListenerState::default());
    let (mut positions_in, mut positions_out) = triple_buffer::triple_buffer(&vec![[0.0f32; 3]; n]);

    let mut dense_x = vec![0.0f32; n];
    let mut dense_y = vec![0.0f32; n];
    let mut dense_z = vec![0.0f32; n];
    let mut local_listener = ListenerState::default();

    let positions: Vec<[f32; 3]> = (0..n)
        .map(|i| [i as f32, i as f32 * 0.5, i as f32 * 0.25])
        .collect();
    let setup_after = AllocSnapshot::now();

    let mut main_total = 0u128;
    let mut sound_total = 0u128;

    for _ in 0..frames {
        // main: input_buffer に in-place 書き込み → publish
        let t = Instant::now();
        *listener_in.input_buffer_mut() = ListenerState {
            position: [1.0, 2.0, 3.0],
            forward: [0.0, 0.0, -1.0],
            up: [0.0, 1.0, 0.0],
            right: [1.0, 0.0, 0.0],
        };
        listener_in.publish();
        positions_in.input_buffer_mut().copy_from_slice(&positions);
        positions_in.publish();
        main_total += t.elapsed().as_nanos();

        // sound: update して output_buffer を読む
        let t = Instant::now();
        listener_out.update();
        local_listener = *listener_out.output_buffer_mut();
        positions_out.update();
        for (i, v) in positions_out.output_buffer_mut().iter().enumerate() {
            dense_x[i] = v[0];
            dense_y[i] = v[1];
            dense_z[i] = v[2];
        }
        sound_total += t.elapsed().as_nanos();
    }
    let frame_after = AllocSnapshot::now();

    black_box(&dense_x);
    black_box(&local_listener);
    BenchResult {
        main_ns: main_total,
        sound_ns: sound_total,
        setup_alloc: setup_after.diff(setup_before),
        frame_alloc: frame_after.diff(setup_after),
        static_bytes: std::mem::size_of::<ListenerState>() * 3
            + 3 * n * std::mem::size_of::<[f32; 3]>(),
    }
}

// =========================================================================

struct BenchResult {
    main_ns: u128,
    sound_ns: u128,
    setup_alloc: AllocDelta,
    frame_alloc: AllocDelta,
    /// 設計上必要な常駐メモリの理論値（ヘッダや内部構造は含まない大まかな目安）
    static_bytes: usize,
}

fn fmt(ns: u128, frames: usize) -> String {
    format!(
        "{:>11} ns total  ({:>8.1} ns/frame)",
        ns,
        ns as f64 / frames as f64
    )
}

fn run(label: &str, frames: usize, f: impl Fn() -> BenchResult) {
    // warmup
    let _ = f();
    let r = f();
    println!("  {label}");
    println!("    time   main : {}", fmt(r.main_ns, frames));
    println!("    time   sound: {}", fmt(r.sound_ns, frames));
    println!("    time   total: {}", fmt(r.main_ns + r.sound_ns, frames));
    println!("    memory static : ~{:>6} B", r.static_bytes);
    println!(
        "    memory setup  : {:>3} allocs, {:>7} B",
        r.setup_alloc.allocs, r.setup_alloc.bytes
    );
    let per_frame_allocs = r.frame_alloc.allocs as f64 / frames as f64;
    let per_frame_bytes = r.frame_alloc.bytes as f64 / frames as f64;
    println!(
        "    memory /frame : {:>5.2} allocs ({:>5} total), {:>6.1} B/frame ({:>8} B total)",
        per_frame_allocs, r.frame_alloc.allocs, per_frame_bytes, r.frame_alloc.bytes
    );
}

fn main() {
    println!("Command enum size: {} B", std::mem::size_of::<Command>());
    println!(
        "ListenerState size: {} B",
        std::mem::size_of::<ListenerState>()
    );

    let frames = 10_000;
    for &n in &[16usize, 64, 256] {
        println!("\n=== sources = {n}, frames = {frames} ===");
        run("Path A: Command ring buffer (現状)", frames, || {
            bench_command(n, frames)
        });
        run("Path B: ArcSwap<Vec> (alloc per frame)", frames, || {
            bench_arcswap(n, frames)
        });
        run("Path C: direct SoA write (理想下限)", frames, || {
            bench_direct(n, frames)
        });
        run("Path D: Triple buffer (自前 unsafe)", frames, || {
            bench_triple(n, frames)
        });
        run(
            "Path E: triple_buffer クレート (unsafe なし)",
            frames,
            || bench_triple_crate(n, frames),
        );
    }
}
