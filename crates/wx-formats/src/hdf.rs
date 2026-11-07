//! HDF5 (and NetCDF-4, which is HDF5) read from memory with `hdf5-reader`.
//!
//! A thin layer that turns the reader's typed API into what the decoders
//! need: attributes as numbers or strings, and a dataset as one flat vector
//! in whatever integer or float type the file stores, refused before reading
//! when it would exceed [`MAX_POINTS`](crate::MAX_POINTS).

use anyhow::{Context, Result, anyhow, bail};
use hdf5_reader::messages::datatype::Datatype;
use hdf5_reader::{Attribute, Dataset, Hdf5File, OpenOptions, SliceInfo, SliceInfoElem};
use rayon::prelude::*;

use crate::MAX_POINTS;

const HDF5_MAGIC: &[u8; 8] = b"\x89HDF\r\n\x1a\n";

/// An open HDF5 file.
pub(crate) struct H5 {
    file: Hdf5File,
}

/// A dataset's values in their stored type, row-major.
pub(crate) enum Values {
    U8(Vec<u8>),
    I8(Vec<i8>),
    U16(Vec<u16>),
    I16(Vec<i16>),
    U32(Vec<u32>),
    I32(Vec<i32>),
    F32(Vec<f32>),
    F64(Vec<f64>),
}

macro_rules! each_values {
    ($v:expr, $x:ident => $e:expr) => {
        match $v {
            Values::U8($x) => $e,
            Values::I8($x) => $e,
            Values::U16($x) => $e,
            Values::I16($x) => $e,
            Values::U32($x) => $e,
            Values::I32($x) => $e,
            Values::F32($x) => $e,
            Values::F64($x) => $e,
        }
    };
}

// The macro casts every stored type to f64, a no-op for the f64 arm.
#[allow(clippy::unnecessary_cast)]
impl Values {
    pub fn len(&self) -> usize {
        each_values!(self, v => v.len())
    }

    /// Every value through `f`, in parallel. Integers are passed exactly
    /// (all stored types fit an `f64`).
    pub fn map(&self, f: impl Fn(f64) -> f32 + Sync + Send) -> Vec<f32> {
        each_values!(self, v => v.par_iter().with_min_len(1 << 14).map(|&x| f(x as f64)).collect())
    }

    /// Every value as an `f64`, which holds each stored type exactly.
    pub fn to_f64(&self) -> Vec<f64> {
        each_values!(self, v => v.iter().map(|&x| x as f64).collect())
    }

    /// Value `i` as an `f64`. Panics when out of range, like indexing.
    pub fn at(&self, i: usize) -> f64 {
        each_values!(self, v => v[i] as f64)
    }

    /// How many values satisfy `p`.
    pub fn count(&self, p: impl Fn(f64) -> bool + Sync + Send) -> usize {
        each_values!(self, v => v.par_iter().with_min_len(1 << 14).filter(|&&x| p(x as f64)).count())
    }

    /// The type name, for error messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            Values::U8(_) => "uint8",
            Values::I8(_) => "int8",
            Values::U16(_) => "uint16",
            Values::I16(_) => "int16",
            Values::U32(_) => "uint32",
            Values::I32(_) => "int32",
            Values::F32(_) => "float32",
            Values::F64(_) => "float64",
        }
    }
}

impl H5 {
    pub fn open(bytes: &[u8]) -> Result<H5> {
        if !bytes.starts_with(HDF5_MAGIC) {
            bail!("not an HDF5 file (no HDF5 signature at byte 0)");
        }
        let options = OpenOptions {
            // Every dataset is read once, whole: a chunk cache would only
            // hold memory.
            chunk_cache_bytes: 0,
            // The largest selection a decoder may ask for: a full grid of
            // doubles. Shapes are checked before reading as well.
            max_read_bytes: MAX_POINTS * 8,
            ..OpenOptions::default()
        };
        let file =
            Hdf5File::from_bytes_with_options(bytes, options).map_err(|e| anyhow!("unreadable HDF5 file: {e}"))?;
        Ok(H5 { file })
    }

    pub fn dataset(&self, path: &str) -> Result<Dataset> {
        self.file.dataset(path).map_err(|e| anyhow!("no dataset {path}: {e}"))
    }

    pub fn has_dataset(&self, path: &str) -> bool {
        self.file.dataset(path).is_ok()
    }

    pub fn has_group(&self, path: &str) -> bool {
        self.file.group(path).is_ok()
    }

    /// Attribute `name` of the group or dataset at `path` (`/` for the root).
    pub fn attr(&self, path: &str, name: &str) -> Result<Attribute> {
        let found = if path == "/" {
            self.file.root_group().and_then(|g| g.attribute(name))
        } else {
            match self.file.group(path) {
                Ok(g) => g.attribute(name),
                Err(_) => self.file.dataset(path).and_then(|d| d.attribute(name)),
            }
        };
        found.map_err(|e| anyhow!("no attribute {name} on {path}: {e}"))
    }

    pub fn has_attr(&self, path: &str, name: &str) -> bool {
        self.attr(path, name).is_ok()
    }

    pub fn attr_f64(&self, path: &str, name: &str) -> Result<f64> {
        let a = self.attr(path, name)?;
        let v = a.read_as_f64().map_err(|e| anyhow!("attribute {path}@{name} is not a number: {e}"))?;
        if !v.is_finite() {
            bail!("attribute {path}@{name} is {v}");
        }
        Ok(v)
    }

    /// A numeric attribute, or `None` when it is absent.
    pub fn attr_f64_opt(&self, path: &str, name: &str) -> Result<Option<f64>> {
        if self.has_attr(path, name) { self.attr_f64(path, name).map(Some) } else { Ok(None) }
    }

    pub fn attr_str(&self, path: &str, name: &str) -> Result<String> {
        let a = self.attr(path, name)?;
        let s = a.read_string().map_err(|e| anyhow!("attribute {path}@{name} is not a string: {e}"))?;
        Ok(s.trim_end_matches('\0').trim().to_string())
    }

    pub fn attr_str_opt(&self, path: &str, name: &str) -> Result<Option<String>> {
        if self.has_attr(path, name) { self.attr_str(path, name).map(Some) } else { Ok(None) }
    }

    /// The whole dataset at `path` and its shape, slowest dimension first.
    pub fn read(&self, path: &str) -> Result<(Values, Vec<usize>)> {
        let ds = self.dataset(path)?;
        let shape: Vec<usize> = ds
            .shape()
            .iter()
            .map(|&d| usize::try_from(d).map_err(|_| anyhow!("{path}: dimension {d} too large")))
            .collect::<Result<_>>()?;
        let n = shape
            .iter()
            .try_fold(1usize, |acc, &d| acc.checked_mul(d))
            .filter(|&n| n <= MAX_POINTS)
            .ok_or_else(|| anyhow!("{path}: shape {shape:?} is above the {MAX_POINTS} point limit"))?;
        let values = read_values(&ds, None).with_context(|| format!("reading {path}"))?;
        if values.len() != n {
            bail!("{path}: read {} values for shape {shape:?}", values.len());
        }
        Ok((values, shape))
    }

    /// Part of the dataset at `path`: one index or range per dimension,
    /// slowest first (`Slice` ends are exclusive), row-major.
    pub fn read_slice(&self, path: &str, selection: &[SliceInfoElem]) -> Result<Values> {
        let ds = self.dataset(path)?;
        let selection = SliceInfo { selections: selection.to_vec() };
        read_values(&ds, Some(&selection)).with_context(|| format!("reading part of {path}"))
    }
}

fn read_values(ds: &Dataset, selection: Option<&SliceInfo>) -> Result<Values> {
    fn flat<T: hdf5_reader::H5Type>(ds: &Dataset, selection: Option<&SliceInfo>) -> Result<Vec<T>> {
        let a = match selection {
            None => ds.read_array_parallel::<T>(),
            Some(s) => ds.read_slice::<T>(s),
        }
        .map_err(|e| anyhow!("{e}"))?;
        let (v, offset) = a.into_raw_vec_and_offset();
        if offset.is_some_and(|o| o != 0) {
            bail!("dataset came back with a non-zero offset");
        }
        Ok(v)
    }
    let s = selection;
    Ok(match ds.dtype() {
        Datatype::FixedPoint { size: 1, signed: false, .. } => Values::U8(flat(ds, s)?),
        Datatype::FixedPoint { size: 1, signed: true, .. } => Values::I8(flat(ds, s)?),
        Datatype::FixedPoint { size: 2, signed: false, .. } => Values::U16(flat(ds, s)?),
        Datatype::FixedPoint { size: 2, signed: true, .. } => Values::I16(flat(ds, s)?),
        Datatype::FixedPoint { size: 4, signed: false, .. } => Values::U32(flat(ds, s)?),
        Datatype::FixedPoint { size: 4, signed: true, .. } => Values::I32(flat(ds, s)?),
        Datatype::FloatingPoint { size: 4, .. } => Values::F32(flat(ds, s)?),
        Datatype::FloatingPoint { size: 8, .. } => Values::F64(flat(ds, s)?),
        other => bail!("unsupported element type {other:?}"),
    })
}
