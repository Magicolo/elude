use core::{alloc::Layout, ptr::NonNull};

pub mod memory;
pub mod simple_memory;

/// TODO: Memory allocator.
///
/// QUESTIONS:
/// - Support for arbitrary types as columns?
///     - At least some standard types like `Box<T>`, `Vec<T>`, etc.
///     - `Box<T>` has a size of 8 or 16 (fat pointer) bytes, which is
///       consistent with other column types (`u64`, `u128`).
///     - `Vec<T>` has a size of 24 bytes... with memory pages of 4kB, there'd
///       be only 170 rows in the table. Also, 24 is an awkward divider of
///       powers or 2.
///         - Could be stored as 16 bytes with `{ len: u32, cap: u32 }`?
/// - How to efficiently join queries?
///     - When possible, the join mechanism must use indexes.
/// - How to define indexes?
/// - Support for commutative write operations with looser locking semantics?
///     - Multiple `Add` or `Multiply` operations do not need an exclusive write
///       lock.
///     - When the operation for a given column changes (say from `Set` to `Add`
///       or `Add` to `Multiply`), the new operation will need to block.
///     - There'd need to be a way to save the current operation for each
///       column.
struct Chunk {
    data: NonNull<u8>,
    // Each bit represents a chunk in the page.
    slots: u32,
    // Next page of the same size.
    next: u32,
}

struct Memory {
    pages: Vec<Chunk>,
    // First page for each size.
    heads: [u32; 5],
    layout: Layout,
}

impl Memory {
    fn new() -> Self {
        let page = page_size::get();
        let layout = unsafe { Layout::from_size_align_unchecked(page, page) };
        Self {
            pages: Vec::new(),
            heads: [u32::MAX; 5],
            layout,
        }
    }
}

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

pub struct Store {
    keys: key::Keys,
    tables: table::Tables,
    indexes: index::Indexes,
    schemas: schema::Schemas,
}

pub mod key {
    pub struct Key;
    pub struct Keys;
}

pub mod index {
    pub struct Index;
    pub struct Indexes;
}

pub mod schema {
    use core::{
        alloc::{Layout, LayoutErr, LayoutError},
        any::TypeId,
        cell::RefCell,
        cmp::Reverse,
        ptr::NonNull,
    };
    use parking_lot::Mutex;
    use triomphe::ThinArc;

    // Header is the maximum row count of each table computed to make a table
    // allocation fit into L2 cache. Aim between 64KiB and 256KiB.
    // Slice is the column definitions.
    #[derive(Clone)]
    pub struct Schema(ThinArc<Header, Column>);

    pub struct Schemas;

    pub struct Header {
        capacity: u32,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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

    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub struct Column {
        r#type: Type,
        path: Path,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub struct Path;

    impl Schema {
        pub fn new(columns: impl IntoIterator<Item = Column>) -> Self {
            let page = page_size::get();
            let mut columns = columns.into_iter().collect::<Vec<_>>();
            columns
                .sort_unstable_by_key(|column| (column.r#type.size(), column.r#type.identifier()));
            columns.dedup_by_key(|column| (column.r#type.size(), column.r#type.identifier()));
            let capacity = columns
                .last()
                .and_then(|column| page.checked_div(column.r#type.size())?.try_into().ok())
                .unwrap_or(u32::MAX);
            Self(ThinArc::from_header_and_iter(
                Header { capacity },
                columns.into_iter(),
            ))
        }
    }

    impl Type {
        #[inline]
        pub const fn identifier(self) -> TypeId {
            match self {
                Self::Bool => TypeId::of::<bool>(),
                Self::Char => TypeId::of::<char>(),
                Self::U8 => TypeId::of::<u8>(),
                Self::U16 => TypeId::of::<u16>(),
                Self::U32 => TypeId::of::<u32>(),
                Self::U64 => TypeId::of::<u64>(),
                Self::U128 => TypeId::of::<u128>(),
                Self::Usize => TypeId::of::<usize>(),
                Self::I8 => TypeId::of::<i8>(),
                Self::I16 => TypeId::of::<i16>(),
                Self::I32 => TypeId::of::<i32>(),
                Self::I64 => TypeId::of::<i64>(),
                Self::I128 => TypeId::of::<i128>(),
                Self::Isize => TypeId::of::<isize>(),
                Self::F32 => TypeId::of::<f32>(),
                Self::F64 => TypeId::of::<f64>(),
            }
        }

        #[inline]
        pub const fn layout(self, count: usize) -> Result<Layout, LayoutError> {
            match self {
                Self::Bool => Layout::array::<bool>(count),
                Self::Char => Layout::array::<char>(count),
                Self::U8 => Layout::array::<u8>(count),
                Self::U16 => Layout::array::<u16>(count),
                Self::U32 => Layout::array::<u32>(count),
                Self::U64 => Layout::array::<u64>(count),
                Self::U128 => Layout::array::<u128>(count),
                Self::Usize => Layout::array::<usize>(count),
                Self::I8 => Layout::array::<i8>(count),
                Self::I16 => Layout::array::<i16>(count),
                Self::I32 => Layout::array::<i32>(count),
                Self::I64 => Layout::array::<i64>(count),
                Self::I128 => Layout::array::<i128>(count),
                Self::Isize => Layout::array::<isize>(count),
                Self::F32 => Layout::array::<f32>(count),
                Self::F64 => Layout::array::<f64>(count),
            }
        }

        #[inline]
        pub const fn size(self) -> usize {
            match self {
                Self::Bool => size_of::<bool>(),
                Self::Char => size_of::<char>(),
                Self::U8 => size_of::<u8>(),
                Self::U16 => size_of::<u16>(),
                Self::U32 => size_of::<u32>(),
                Self::U64 => size_of::<u64>(),
                Self::U128 => size_of::<u128>(),
                Self::Usize => size_of::<usize>(),
                Self::I8 => size_of::<i8>(),
                Self::I16 => size_of::<i16>(),
                Self::I32 => size_of::<i32>(),
                Self::I64 => size_of::<i64>(),
                Self::I128 => size_of::<i128>(),
                Self::Isize => size_of::<isize>(),
                Self::F32 => size_of::<f32>(),
                Self::F64 => size_of::<f64>(),
            }
        }
    }
}

pub mod query {
    /// TODO: To join queries, define `Query::join(on: Path, queries:
    /// (Query<Q0>, Query<Q1>, ...))`.
    ///     - Note that with this algorithm, it should be possible to join any
    ///       number of queries, not just 2.
    ///     - Ensure that the `on` and `queries` do not have read-write or
    ///       write-write conflicts between each other such that they can not
    ///       create deadlocks, UB or inconsistent behavior.
    ///     - Take read locks on all of the join key columns on all tables
    ///       covered by the queries (note that there may be some table overlap
    ///       between queries and that is ok); locks will be taken incrementally
    ///       with a `try_lock` strategy (be sure to avoid deadlocks).
    ///     - Build a map from the keys that intersect all the queries and a
    ///       `Map<Key, (Row(table0, row0), Row(table1, row1), ...)>`.
    ///     - Iterate on the map values and yield the joined rows. Accumulate
    ///       the locks on the visited tables rather than potentially locking
    ///       and unlocking for each yielded row (with a `try_lock` strategy?);
    ///       perhaps keep a count of how many rows will be yielded for each
    ///       table and release the locks for exhausted tables early (including
    ///       the join key lock).
    ///     - Release all remaining table locks, if any.
    struct Query;
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
    .write(Path::new().then(Any).then("position").then(0))
    .write(Path::new().then(Any).then(Any).then("x").cast::<f32>())
    // `Any` will match any root.
    // As soon as a type is provided, strongly typed paths can be used again.
    .read(Path::new().then(Any).then(Vector3::field("position")).x())
    // Static paths might be generated.
    .read(Path::new().then(Any).then(Player::position()))
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
