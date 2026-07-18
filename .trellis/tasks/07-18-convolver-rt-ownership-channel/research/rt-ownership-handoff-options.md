# RT Ownership Hand-off Options

## Required invariants

* 音频线程只执行固定次数原子操作；成本不得随发布者数量、历史线程数或 stream
  长度增长。
* 音频线程不分配、不释放 kernel、不获取锁、不遍历共享链表。
* control 侧继续允许 cloneable 多发布者，但通过现有 control-only mutex 串行化为
  一个逻辑 producer。
* publication 保持 latest-wins；退休容量保持固定有界；满载时继续处理当前 kernel
  并明确 backpressure。
* kernel 的每一次 Box/Arc 析构都能证明发生在 control/offline 线程。
* 一个 control 最多拥有一个 live audio consumer，且由 API 强制而非文档约定。

## Approach A - AtomicPtr ownership slots（推荐）

control 线程把 `Box<PublishedConvolver>` 转成 raw pointer，使用一个 published
`AtomicPtr` 发布；audio 用固定一次 `swap(null)` 取得唯一 ownership。retired 方向
使用另一个单生产者/单消费者 `AtomicPtr`，audio 仅在 CAS-from-null 成功时交还，
control 用 `swap(null)` 取回并 `Box::from_raw`/drop。control-side latest-wins 替换
的旧 published pointer 只在 control 线程释放。

优点：两个方向都是严格 O(1)，无引用计数、Guard、debt node 或外部依赖；与当前
单槽 + `pending_retire` 状态机最接近。缺点：需要一个很小但必须严格审计的 unsafe
ownership 模块，Drop/关闭顺序和 ABA 不变量必须通过类型与测试固定。

## Approach B - 预分配 SPSC ring

为 publish 和 retire 各使用构造期预分配的有界 SPSC ring；control 多发布者先经
mutex 串行化。队列元素持有唯一 ownership，audio pop/push 不分配。

优点：容量和 backpressure 直观，可扩展到未来 kernel transition。缺点：需要引入
并验证 RT-safe queue 依赖或自行实现 ring；latest-wins 仍需 control 侧 coalescing，
队列容量大于一会扩大 heavy-kernel 驻留内存，当前需求实际上只需单槽。

## Approach C - 预分配 index pool + state word

setup 时预分配固定节点池，跨线程只传递 index/generation，kernel ownership 留在
pool 节点中；一个原子 state word 编码 free/published/audio-owned/retired。

优点：可把 consumer lease、generation 和 telemetry 做成单一一致状态机，避免 raw
heap pointer 跨线程。缺点：实现与验证复杂度最高，动态 IR 的最大内存/尺寸难在
setup 时预分配，容易把本次修复扩展成通用资源池。

## Recommendation

采用 Approach A，并把 unsafe 限制在独立 `convolver_control` 模块：两个方向各一
个 ownership slot，audio 本地保留 `owned/incoming/pending_retire` 固定 staging。
同时引入不可克隆 consumer lease；后续 telemetry 使用 generation/state word，
不再读取 eventually-consistent status 做内部决策。若未来确实需要多 kernel
crossfade，再以独立任务评估 SPSC ring，而不是提前扩大容量。

## Detailed topology comparison

### A. AtomicPtr mailbox in both directions

```text
control/offline                         audio consumer

Box::new(kernel)
      |
      v
[published AtomicPtr] -- swap(NULL) --> incoming --> owned/current
      ^                                               |
      |                                               v
latest-wins swap/drop                         pending_retire
on control thread                                    |
                                                     v CAS(NULL, ptr)
control drop <-- swap(NULL) -- [retired AtomicPtr] <--+
```

The published direction has one logical producer because cloned publishers are
serialized by the existing control-only mutex. `publish` converts a
`Box<PublishedConvolver>` to a raw pointer and atomically replaces the mailbox.
If the previous pointer was still waiting, control reconstructs and drops that
Box immediately: latest-wins destruction therefore stays off RT. Audio performs
one `swap(null)` to take unique ownership; there is no guard or reference-count
promotion.

The retired direction reverses producer/consumer roles. Audio attempts only
`compare_exchange(null, ptr)`. On failure it keeps the Box in its fixed
`pending_retire` field; it never drops the Box. Control performs `swap(null)`,
reconstructs the Box, and destroys it. Release publication plus Acquire take is
enough to publish initialized kernel state; AcqRel can be used uniformly for the
exchange operations.

ABA is limited because CAS compares only against null and no thread retains a
borrow after ownership transfer. A reused non-null address is never used as the
expected value. The unsafe proof still must cover exactly-once `Box::from_raw`,
slot cleanup on Drop, stopped publishers before shutdown, and processor/control
destruction off RT.

Backpressure remains identical to the current design: one retired mailbox plus
audio-local `pending_retire`. When both are occupied, the active kernel keeps
processing and a newly withdrawn kernel stays in `incoming` until control
drains. Publication itself remains latest-wins before withdrawal.

Expected hot-path synchronization cost is a fixed handful of pointer atomics
and scalar telemetry atomics. It is independent of the number of threads that
ever touched the control.

### B. Preallocated SPSC rings

```text
control Producer --> [publish ring: N slots] --> audio Consumer
control Consumer <-- [retire ring:  N slots] <-- audio Producer
```

Construction allocates both rings; push/pop later move a uniquely owned
`Box<PublishedConvolver>` without allocation. Non-clone Producer/Consumer halves
naturally enforce one audio consumer. Cloned control publishers would share the
control Producer behind the existing off-RT mutex, while audio owns the only
publish Consumer and retire Producer.

The difficult part is latest-wins. A conventional SPSC producer cannot remove
or overwrite an unread element. With capacity one, a full publish ring changes
the current contract from latest-wins to reject/defer-newest. With capacity N,
audio may adopt stale intermediate kernels and heavy-kernel residency grows to
N. A separate control-side coalescing mailbox can restore latest-wins, but that
mailbox recreates most of Approach A. An overwrite-capable ring needs a more
specialized algorithm and equally careful destruction-thread auditing.

The retire ring can absorb more control-thread scheduling jitter than a single
slot, but this is a policy change: it parks more FFT kernels and increases the
fixed memory ceiling. Queue Drop must also be forced off RT because queued
elements are destroyed by whichever thread drops the relevant storage.

This approach is attractive when ordered multi-kernel transitions are an actual
requirement. For the current single-current/single-latest contract, a ring adds
states without improving normal behavior.

### C. Fixed index pool plus atomic state word

```text
slot state: FREE -> WRITING -> PUBLISHED -> AUDIO_OWNED -> RETIRED -> FREE
                              \-> SUPERSEDED ----------------------/

shared atomic word: generation + published index + consumer lease/state
fixed slots: UnsafeCell<Option<Box<PublishedConvolver>>> x N
```

Control writes a Box into a free slot, then publishes its index/generation.
Audio changes only slot states/index ownership and processes the Box in place;
control reclaims RETIRED or SUPERSEDED slots and performs every drop. No raw heap
pointer crosses the API boundary, and one versioned state word can make lease,
publication and quiescence snapshots coherent.

However, the Box is still dynamically allocated because FFTConvolver size
depends on IR length. The pool only preallocates metadata and Box holders, not
the heavy kernel memory. It needs a formal transition table, `UnsafeCell`
safety proof, generation-wrap policy, slot-count proof, error recovery for every
intermediate state, and either fixed scans or an atomic free-list. A four-slot
pool roughly models current/current-incoming/pending-retire/retired, but each
extra future transition increases the state space.

This is justified if the project wants a reusable real-time resource pool or a
single linearizable lifecycle snapshot. It is disproportionate if the goal is
only replacing two ArcSwap mailboxes.

## Comparison matrix

| Dimension | A: AtomicPtr slots | B: SPSC rings | C: Index pool |
| --- | --- | --- | --- |
| RT worst-case | Strict O(1), minimum atomics | Strict O(1) push/pop | O(1) with bitset, or fixed O(N) scan |
| Audio alloc/free | None by design | None after ring construction | None after pool construction |
| Latest-wins | Natural control-side swap | Awkward without overwrite/coalescer | Explicit state transition |
| Backpressure memory | Current fixed bound | Configurable but larger N | Fixed pool size N |
| Single consumer | Add explicit lease CAS | Natural in split halves | Encode in state word |
| Local unsafe | Small raw-Box boundary | Usually none beyond dependency | Largest UnsafeCell/state proof |
| New dependency | No | Usually yes | No |
| Shutdown complexity | Moderate, two slots + local stages | Moderate, drain two rings | High, every slot/state must settle |
| Future ordered crossfade | New design required | Best fit | Possible but state-heavy |
| Fit for current contract | Best | Acceptable but overbuilt | Technically strong, overbuilt |

## Consumer lease interaction

All three approaches should make consumer acquisition explicit. The recommended
surface is a cloneable `ConvolverPublisher`/control facade plus a non-cloneable
`ConvolverConsumerLease`. `try_acquire_consumer` uses a CAS and returns a typed
`ConsumerAlreadyActive` error. `ConvolverProcessor` consumes the lease by value;
callback and render builders cannot silently clone it. Releasing the lease is
allowed only after publishers stop and lifecycle status is quiescent, with the
processor and lease themselves destroyed off RT.

Approach B obtains much of this structurally from SPSC halves. Approach A still
needs one consumer-active atomic. Approach C can fold lease state into its
versioned state word, at the cost of coupling more transitions.

## Shutdown protocol shared by all approaches

1. Stop all publishers and prevent new consumer construction.
2. Disable the processor, but if finish has begun, first drain its locked finite tail.
3. Drive process/repeated finish until audio owns no current/incoming/pending-retire value.
4. Drain every retired value on control/offline and confirm no pending publication.
5. Observe a coherent quiescent state, release the consumer lease, then destroy processor/control off RT.

The hand-off primitive alone cannot make dropping an entire processor inside an
audio callback safe. The API and tests must continue to require processor/control
teardown on a non-realtime thread.
