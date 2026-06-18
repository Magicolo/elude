pub mod memory;
pub mod simple_memory;

pub trait Table {
    type Store;
    type Read<'a>
    where
        Self: 'a;
    type Write<'a>
    where
        Self: 'a;
}

pub trait Column {}

pub struct Vector3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

pub struct Player {
    pub position: Vector3,
    pub velocity: Vector3,
    pub health: f32,
    pub mass: f32,
    pub status: u8,
}

pub struct Enemy {
    pub position: Vector3,
    pub velocity: Vector3,
}

pub mod player {
    use super::*;

    #[derive(Debug, Copy, Clone, Default)]
    pub struct Player<Position, Velocity> {
        pub position: Position,
        pub velocity: Velocity,
    }

    impl Table for super::Player {
        type Read<'a> = Player<<Vector3 as Table>::Read<'a>, <Vector3 as Table>::Read<'a>>;
        type Store = Player<<Vector3 as Table>::Store, <Vector3 as Table>::Store>;
        type Write<'a> = Player<<Vector3 as Table>::Write<'a>, <Vector3 as Table>::Write<'a>>;
    }
}

pub mod vector3 {
    use super::*;

    #[derive(Debug, Copy, Clone, Default)]
    pub struct Vector3<X, Y, Z> {
        pub x: X,
        pub y: Y,
        pub z: Z,
    }

    impl Table for super::Vector3 {
        type Read<'a> =
            Vector3<<f32 as Table>::Read<'a>, <f32 as Table>::Read<'a>, <f32 as Table>::Read<'a>>;
        type Store = Vector3<<f32 as Table>::Store, <f32 as Table>::Store, <f32 as Table>::Store>;
        type Write<'a> = Vector3<
            <f32 as Table>::Write<'a>,
            <f32 as Table>::Write<'a>,
            <f32 as Table>::Write<'a>,
        >;
    }
}

impl<C: Column> Table for C {
    type Read<'a>
        = &'a [C]
    where
        Self: 'a;
    type Store = Vec<C>;
    type Write<'a>
        = &'a mut [C]
    where
        Self: 'a;
}

impl Column for usize {}
impl Column for f32 {}

pub mod schema {
    use core::alloc::Layout;
    use triomphe::ThinArc;

    // Header is the maximum row count of each table computed to make a table
    // allocation fit into L2 cache. Aim between 64KiB and 256KiB.
    // Slice is the column definitions.
    #[derive(Clone)]
    pub struct Schema(ThinArc<Header, Column>);

    pub struct Header {
        capacity: u32,
    }

    #[derive(Clone, Copy)]
    pub enum Type {
        Bool,
        Char,
        U8,
        U16,
        U32,
        U64,
        U128,
        Usize,
        I8,
        I16,
        I32,
        I64,
        I128,
        Isize,
        F32,
        F64,
    }

    pub struct Column {
        r#type: Type,
        path: Path,
    }

    pub struct Path;

    impl Schema {
        pub fn layout(&self, count: u32) -> Layout {
            todo!()
        }
    }
}

pub mod table {
    /// TODO: Use `NonNull<usize>` instead of `NonNull<u8>` to prove alignment
    /// to the compiler. Copying is supposedly faster.
    /// TODO: Memory page size can be retrieved with the `page_size` crate.
    /// TODO: Allocate columns independently in chunks that
    /// correspond to memory pages (4kB).
    ///     - For 1 byte typed columns -> 4096 rows
    ///     - For 2 bytes typed columns -> 2048 rows
    ///     - For 4 bytes typed columns -> 1024 rows
    ///     - For 8 bytes typed columns -> 512 rows
    ///     - For 16 bytes typed columns -> 256 rows
    ///     - Some platform use a 16kB page size, si the allocator may split the
    ///       pages in 4.
    ///     - These neatly correspond to concurrent units.
    ///     - Always allocate in chunks of 4kB:
    ///         - Determine the row count based on `4kB / largest data type
    ///           size`.
    ///         - Data types that are smaller than the largest may combine into
    ///           a single page.
    ///         - Combine join columns to fill the 4kB.
    ///     - This layout logic will be pre-computed in the `Schema`.
    ///     - This reduces the cost of copying from/to write buffers.
    ///     - Write buffers will also use full page allocations. Although they
    ///       might not fill it, it is guaranteed to be large enough for a
    ///       single column.
    use crate::schema::Schema;
    use arc_swap::ArcSwapAny;
    use core::ptr::NonNull;
    use parking_lot::{Condvar, Mutex};
    use triomphe::ThinArc;

    pub struct Tables(ArcSwapAny<ThinArc<(), Table>>);

    #[derive(Clone)]
    pub struct Table(ThinArc<Header, usize>);

    pub struct Header {
        schema: Schema,
        state: Mutex<State>,
        read: Condvar,
        write: Condvar,
    }

    /// - Queries can use a `try_lock` pattern on first attempt at locking their
    ///   tables and change the order of iteration when failing. On second
    ///   attempt, they would use a stricter `lock`.
    ///
    /// - `state.read_mask` and `state.write_mask` are aliased bit masks that
    ///   represent the usage of the columns. They are aliased because if there
    ///   is more than 64 columns, the bit 0 will lock columns [0, 64, 128, ...]
    ///   and the bit 11 will lock columns [11, 75, 139, ...]. In general, we
    ///   expect tables to have less than 64 columns.
    ///
    /// - ISSUE: Writers have to signal copy-back intent to readers somehow.
    ///   Otherwise, if there is a steady stream of readers, `state.read_count`
    ///   may never reach 0, thus never reset `state.read_mask`. If writers
    ///   signal their intent, readers could be able to wait for write
    ///   resolution. Add another `copy_mask: u64`?
    /// - For each table of a given query:
    ///     - It locks the table state (may try to lock and go for another table
    ///       on failure).
    ///     - While `state.write_mask & write_mask > 0 || state.copy_mask &
    ///       read_mask > 0`: `header.write.wait()` (or may try another table).
    ///     - It `state.write_mask |= write_mask`
    ///     - It `state.read_mask |= read_mask`
    ///     - If `read_mask > 0`, it `state.read_count += 1`.
    ///     - It reads `state.data` and `state.count`.
    ///     - It unlocks the table state.
    ///     - Copies write columns to local write buffers.
    ///     - Does its work.
    ///     - It locks the table state.
    ///     - If `read_mask > 0`, it `state.read_count -= 1`.
    ///     - If `state.read_count == 0`, it `state.read_mask = 0` (read bits
    ///       are only reset when there are no readers left; this is to spare
    ///       needing a counter for each column).
    ///     - It `state.copy_mask |= write_mask`.
    ///     - While `state.read_mask & write_mask > 0`: `header.read.wait()`.
    ///     - If `write_mask > 0`, the local write buffers are copied into
    ///       storage.
    ///     - It `state.copy_mask &= ~write_mask`.
    ///     - It unlocks the table state.
    ///     - If `read_mask > 0` and has reset `read_count`, it
    ///       `header.read.notify_all()`.
    ///     - If `write_mask > 0`, it `header.write.notify_all()`.
    ///
    /// - Only the first table of a given schema will be resizable. This is to
    ///   prevent small table to have a full say 1024 rows. Tables will be
    ///   restricted with a maximum capacity (is parameterizable) to favor
    ///   parallelism.
    ///
    /// - For batch insertions in this table:
    ///     - It locks the state (may try to lock and try another elligible
    ///       table on failure if one exists).
    ///     - If `state.count + insertion_count > state.capacity` and the table
    ///       can be resized (has not reached it maximum size):
    ///         - It allocates the new capacity.
    ///         - It copies the data to it.
    ///         - It updates `state.capacity` and `state.data`.
    ///         - It sets `state.read_mask = u64::MAX` to prevent writers from
    ///           copying to storage.
    ///         - TODO: Set `state.write_mask` and `state.copy_mask`?
    ///         - While `state.read_count > 0`, `header.read.wait()`.
    ///         - It deallocates the old data.
    ///         - It resets `state.read_mask = 0`.
    ///         - (do I need to `header.write.notify_all()` here?)
    ///     - It copies items that fit the table to their columns.
    ///     - It `state.count += inserted_count`.
    ///     - It unlocks the state.
    ///     - If items remain, go to next table with the same schema.
    ///     - If items remain and no other table exist for this schema, create a
    ///       new non-resizable table with the same `state.capacity` as this one
    ///       and insert into it.
    ///
    /// - ISSUE: The incremental lock protocol can cause deadlocks. Fine if bits
    ///   are locked in order.
    /// - For batch removals in this table:
    ///     - It locks the state.
    ///     - If `state.write_mask > 0`, `header.write.wait()` and on every wake
    ///       up, lock all available write bits *in order* (if the next bit to
    ///       be locked is already taken, wait; order prevents deadlocks) until
    ///       they have been all locked. Also set the corresponding
    ///       `state.copy_mask` bits to also block readers.
    ///     - While `state.read_count > 0`, `header.read.wait()`.
    ///     - It does all its `swap_remove` operations.
    ///     - It updates `state.count -= removed_count`.
    ///     - It sets `state.write_mask = 0`.
    ///     - It unlocks the state.
    pub struct State {
        count: u32,
        capacity: u32,
        read_count: u32,
        read_mask: u64,
        write_mask: u64,
        copy_mask: u64,
        data: NonNull<u8>,
    }

    // impl Header {
    //     pub fn lock(&self, mask: u64) {
    //         // TODO: Check ordering.
    //         let mut old = self.lock.load(Ordering::Relaxed);
    //         loop {
    //             if old & mask == 0 {
    //                 match self.lock.compare_exchange_weak(
    //                     old,
    //                     old | mask,
    //                     Ordering::Relaxed,
    //                     Ordering::Relaxed,
    //                 ) {
    //                     Ok(_) => break,
    //                     Err(new) => old = new,
    //                 }
    //             }
    //             // TODO: Wait. Use `atomic-wait` and an `AtomicU32`?
    //         }
    //     }

    //     pub fn try_lock(&self, mask: u64) -> bool {
    //         // TODO: Check ordering.
    //         let old = self.lock.load(Ordering::Relaxed);
    //         if old & mask == 0 {
    //             match self.lock.compare_exchange_weak(
    //                 old,
    //                 old | mask,
    //                 Ordering::Relaxed,
    //                 Ordering::Relaxed,
    //             ) {
    //                 Ok(_) => true,
    //                 Err(_) => false,
    //             }
    //         } else {
    //             false
    //         }
    //     }

    //     pub unsafe fn unlock(&self, mask: u64) {
    //         self.lock.fetch_and(!mask, Ordering::Relaxed);
    //     }
    // }

    impl State {
        pub fn boba(&mut self) {}
    }
}

/*
let store = Store::new();
let schema = Schema::builder()
    .column(Path::player().position().x())
    .column(Path::player().position().y())
    .column(Path::player().position().z())
    .column(Path::player().velocity().x())
    .column(Path::player().velocity().y())
    .column(Path::player().velocity().z())
    // Will get or add the table to the store based on the definition.
    .build()?;
let table = store.tables().get_or_insert(&schema);


// `Query<Q, F>` will use the `IntoFlat/Nest` pattern.
let mut query = Query::builder()
    // Implementing a `Table` adds an extension to `Path` with strongly typed paths.
    // `.field` and `.at` can still be used.
    // `Path<T>` will carry the type of the column to query and may resolve to `Any`.
    .write(Path::player().velocity().z())
    // Paths can as dynamic as they need to.
    // Field can also be indexed; useful for tuple structures.
    // - Using a `&str` is a shorthand for `Any::field("position")`.
    // - Using a `usize` is a shorthand for `Any::index(0)`.
    .write(Path::any().then("position").then(0))
    // `Path::any` will match any root.
    // As soon as a type is provided, strongly typed paths can be used again.
    .read(Path::any().then(Vector3::field("position")).x())
    // Static paths might be generated.
    .read(Path::any().then(Player::position()))
    // `try_` variants will yield `Some/None` based on the presence of the column.
    .try_write(Path::any().then(Vector3::index(0)).x())
    .has(Path::any().field("health"))
    .not(Path::any().field("invincible"))
    .build(&store)?;

for table in query.tables() {
    for columns in table.columns() {
        let (a, b, c, d) = columns.get();
        for (a, b, c, d) in izip!(a, b, c, d) {
            *a = *b + *c - *d;
        }
    }
}
 */

fn _boba() {
    let _a = <Player as Table>::Store::default();
}
