use std::ffi::{c_char, CStr, CString};
use std::fmt;
use std::ops::{Deref, Index};
use std::ptr::NonNull;

pub struct CStringSlice([*const c_char]);

impl CStringSlice {
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn as_raw_slice(&self) -> &[*const c_char] {
        // SAFETY: NonNull<c_char> can be safely transmuted to *const c_char
        unsafe { self.0.align_to::<*const c_char>().1 }
    }

    /// Create a new slice from its raw parts.
    ///
    /// # Safety
    /// * All the same preconditions as [`std::slice::from_raw_parts`] apply.
    /// * All pointers in the resulting slice must be non-null.
    pub unsafe fn from_raw_parts<'a>(ptr: *const *const c_char, len: usize) -> &'a Self {
        unsafe { Self::from_raw_slice(std::slice::from_raw_parts(ptr, len)) }
    }

    /// Create a [`CStringSlice`] from a slice of raw pointers.
    ///
    /// # Safety
    /// * All pointers in the resulting slice must be non-null.
    pub unsafe fn from_raw_slice(slice: &[*const c_char]) -> &Self {
        unsafe { &*(slice as *const [*const c_char] as *const CStringSlice) }
    }

    pub fn iter(&self) -> CStringListIter<'_> {
        CStringListIter {
            list: self.as_raw_slice().iter(),
        }
    }
}

impl fmt::Debug for CStringSlice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl Index<usize> for CStringSlice {
    type Output = CStr;

    fn index(&self, index: usize) -> &Self::Output {
        unsafe { CStr::from_ptr(self.0[index]) }
    }
}

#[derive(Default)]
pub struct CStringList(Vec<NonNull<c_char>>);

impl CStringList {
    pub const fn new() -> Self {
        Self(Vec::new())
    }

    pub fn as_slice(&self) -> &CStringSlice {
        // SAFETY: NonNull<c_char> is layout compatible with *const c_char.
        let slice = unsafe { self.0.as_slice().align_to::<*const c_char>().1 };

        // SAFETY: We guarantee that all pointers in the slice are non-null.
        unsafe { CStringSlice::from_raw_slice(slice) }
    }

    pub fn push_back(&mut self, value: CString) {
        self.extend(std::iter::once(value));
    }
}

impl Drop for CStringList {
    fn drop(&mut self) {
        for s in self.0.drain(..) {
            let _ = unsafe { CString::from_raw(s.as_ptr()) };
        }
    }
}

impl Deref for CStringList {
    type Target = CStringSlice;

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl fmt::Debug for CStringList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_slice().fmt(f)
    }
}

impl Extend<CString> for CStringList {
    fn extend<T: IntoIterator<Item = CString>>(&mut self, iter: T) {
        self.0.extend(
            iter.into_iter()
                .map(|item| unsafe { NonNull::new_unchecked(item.into_raw()) }),
        );
    }
}

impl FromIterator<CString> for CStringList {
    fn from_iter<T: IntoIterator<Item = CString>>(iter: T) -> Self {
        let mut list = Self::new();
        list.extend(iter);
        list
    }
}

impl<'a> Extend<&'a CStr> for CStringList {
    fn extend<T: IntoIterator<Item = &'a CStr>>(&mut self, iter: T) {
        self.extend(iter.into_iter().map(|s| s.to_owned()))
    }
}

impl<'a> FromIterator<&'a CStr> for CStringList {
    fn from_iter<T: IntoIterator<Item = &'a CStr>>(iter: T) -> Self {
        Self::from_iter(iter.into_iter().map(|s| s.to_owned()))
    }
}

pub struct CStringListIter<'a> {
    list: std::slice::Iter<'a, *const c_char>,
}

impl<'a> Iterator for CStringListIter<'a> {
    type Item = &'a CStr;

    fn next(&mut self) -> Option<Self::Item> {
        self.list
            .next()
            .copied()
            .map(|p| unsafe { CStr::from_ptr(p) })
    }
}

impl<'a> IntoIterator for &'a CStringList {
    type IntoIter = CStringListIter<'a>;
    type Item = &'a CStr;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a> IntoIterator for &'a CStringSlice {
    type IntoIter = CStringListIter<'a>;
    type Item = &'a CStr;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
