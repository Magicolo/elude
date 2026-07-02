<div align="center"> <h1> elude 0.1.0 </h1> </div>

<p align="center">
    <em>
Columnar data library with a lock-free page pool, type-level schema system, and columnar table layout.
    </em>
</p>

<div align="right">
    <a href="https://github.com/magicolo/elude/actions/workflows/test.yml"> <img src="https://github.com/magicolo/elude/actions/workflows/test.yml/badge.svg"> </a>
    <a href="https://crates.io/crates/elude"> <img src="https://img.shields.io/crates/v/elude.svg"> </a>
</div>

---

### In Brief

- Lock-free page pool with per-thread Treiber stacks and local caches.
- Schema system with pre-computed page layouts based on column type sizes.
- Columnar table storage with concurrent read/write/copy-back protocol.

_See the [examples](examples/) and [tests](tests/) folder for more detailed examples._

---

### Contribute

- If you find a bug or have a feature request, please open an [issue](https://github.com/magicolo/elude/issues).
- `elude` is actively maintained and [pull requests](https://github.com/magicolo/elude/pulls) are welcome.
- If `elude` was useful to you, please consider leaving a [star](https://github.com/magicolo/elude)!
