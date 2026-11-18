//! Windows' own spell checker (the Spell Checking API behind Notepad and
//! Edge: `ISpellCheckerFactory` / `ISpellChecker`, offline, with the
//! user's dictionaries), called through its COM interfaces by hand.
//!
//! Every `unsafe` block of spell checking is here: each call is a thin
//! method with `SAFETY` notes.  The objects belong to the thread that made
//! them (the spell-check worker, see `spell`); they are not `Send`.

use std::{ffi::c_void, ptr::NonNull};

use crate::spell::{Checker, Found};

/// A COM GUID.
#[repr(C)]
#[allow(dead_code)] // Read by Windows.
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

impl Guid {
    const fn from_u128(value: u128) -> Self {
        Self {
            data1: (value >> 96) as u32,
            data2: (value >> 80) as u16,
            data3: (value >> 64) as u16,
            data4: (value as u64).to_be_bytes(),
        }
    }
}

/// `CLSID_SpellCheckerFactory`.
const CLSID_SPELL_CHECKER_FACTORY: Guid =
    Guid::from_u128(0x7ab3_6653_1796_484b_bdfa_e74f_1db7_c1dc);
/// `IID_ISpellCheckerFactory`.
const IID_SPELL_CHECKER_FACTORY: Guid = Guid::from_u128(0x8e01_8a9d_2415_4677_bf08_794e_a61f_94bb);
/// `CLSCTX_INPROC_SERVER`.
const CLSCTX_INPROC_SERVER: u32 = 0x1;
/// `COINIT_MULTITHREADED`.
const COINIT_MULTITHREADED: u32 = 0x0;
/// `S_OK`: the only answer meaning "here is one more" from `Next`.
const S_OK: i32 = 0;
/// `CORRECTIVE_ACTION_DELETE`: a repeated word.
const CORRECTIVE_ACTION_DELETE: i32 = 3;
/// `LOCALE_NAME_MAX_LENGTH`.
const LOCALE_NAME_MAX_LENGTH: i32 = 85;
/// Suggestions asked for per word.
const MAX_SUGGESTIONS: usize = 6;

type Hresult = i32;
type This = *mut c_void;

#[repr(C)]
struct UnknownVtbl {
    _query_interface: usize,
    _add_ref: usize,
    release: unsafe extern "system" fn(This) -> u32,
}

#[repr(C)]
struct FactoryVtbl {
    _base: UnknownVtbl,
    _supported_languages: usize,
    is_supported: unsafe extern "system" fn(This, *const u16, *mut i32) -> Hresult,
    create_spell_checker: unsafe extern "system" fn(This, *const u16, *mut This) -> Hresult,
}

#[repr(C)]
struct CheckerVtbl {
    _base: UnknownVtbl,
    _language_tag: usize,
    check: unsafe extern "system" fn(This, *const u16, *mut This) -> Hresult,
    suggest: unsafe extern "system" fn(This, *const u16, *mut This) -> Hresult,
    add: unsafe extern "system" fn(This, *const u16) -> Hresult,
    ignore: unsafe extern "system" fn(This, *const u16) -> Hresult,
}

#[repr(C)]
struct EnumErrorsVtbl {
    _base: UnknownVtbl,
    next: unsafe extern "system" fn(This, *mut This) -> Hresult,
}

#[repr(C)]
struct ErrorVtbl {
    _base: UnknownVtbl,
    start_index: unsafe extern "system" fn(This, *mut u32) -> Hresult,
    length: unsafe extern "system" fn(This, *mut u32) -> Hresult,
    corrective_action: unsafe extern "system" fn(This, *mut i32) -> Hresult,
}

#[repr(C)]
struct EnumStringVtbl {
    _base: UnknownVtbl,
    next: unsafe extern "system" fn(This, u32, *mut *mut u16, *mut u32) -> Hresult,
}

#[allow(unsafe_code)]
#[link(name = "ole32")]
unsafe extern "system" {
    fn CoInitializeEx(reserved: *const c_void, co_init: u32) -> Hresult;
    fn CoCreateInstance(
        clsid: *const Guid,
        outer: *mut c_void,
        context: u32,
        iid: *const Guid,
        out: *mut This,
    ) -> Hresult;
    fn CoTaskMemFree(memory: *const c_void);
}

#[allow(unsafe_code)]
unsafe extern "system" {
    fn GetUserDefaultLocaleName(name: *mut u16, length: i32) -> i32;
}

/// An owned COM interface pointer whose vtable is `V`; released on drop.
struct Com<V> {
    this: NonNull<c_void>,
    _vtbl: std::marker::PhantomData<V>,
}

impl<V> Com<V> {
    /// Take ownership of `this` (from a call that handed out a reference).
    fn from_raw(this: This) -> Option<Self> {
        NonNull::new(this).map(|this| Self {
            this,
            _vtbl: std::marker::PhantomData,
        })
    }

    const fn this(&self) -> This {
        self.this.as_ptr()
    }

    /// The object's vtable.
    #[allow(unsafe_code)]
    const fn vtbl(&self) -> &V {
        // SAFETY: a COM object starts with a pointer to its vtable, which
        // lives as long as the object; `V` matches the interface this
        // pointer was asked for (its methods in declaration order).
        unsafe { &**self.this.as_ptr().cast::<*const V>() }
    }
}

impl<V> Drop for Com<V> {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: every vtable starts with IUnknown's; this pointer holds
        // one reference, given back here once.
        unsafe {
            let unknown = &**self.this.as_ptr().cast::<*const UnknownVtbl>();
            (unknown.release)(self.this.as_ptr());
        }
    }
}

/// `text` as a NUL-terminated UTF-16 string.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// Read and free a string the API allocated with `CoTaskMemAlloc`.
#[allow(unsafe_code)]
fn take_string(text: *mut u16) -> Option<String> {
    if text.is_null() {
        return None;
    }
    // SAFETY: the API returns NUL-terminated UTF-16 strings; the length is
    // found before reading, and the memory is freed once, after reading.
    unsafe {
        let mut len = 0;
        while *text.add(len) != 0 {
            len += 1;
        }
        let value = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
        CoTaskMemFree(text.cast());
        Some(value)
    }
}

/// The user's language (`en-US`, …).
#[allow(unsafe_code)]
fn user_language() -> Option<String> {
    let mut name = [0u16; LOCALE_NAME_MAX_LENGTH as usize];
    // SAFETY: the buffer and its length in UTF-16 units are valid; the
    // function writes at most that many, NUL included.
    let len = unsafe { GetUserDefaultLocaleName(name.as_mut_ptr(), LOCALE_NAME_MAX_LENGTH) };
    let len = usize::try_from(len).ok()?.checked_sub(1)?;
    Some(String::from_utf16_lossy(&name[..len]))
}

/// A spell checker for the user's language (or US English).
pub(crate) struct WinSpellChecker {
    checker: Com<CheckerVtbl>,
}

impl WinSpellChecker {
    /// Start COM on this thread and make the checker; `None` when Windows
    /// has none (no checker for the language, or COM failed).
    #[allow(unsafe_code)]
    pub(crate) fn new() -> Option<Self> {
        // SAFETY: called on the worker thread before any other COM call
        // there; a second call on the same thread is harmless.
        let _ = unsafe { CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED) };
        let mut factory: This = std::ptr::null_mut();
        // SAFETY: valid GUIDs and out pointer; on success `factory` holds
        // one reference to an `ISpellCheckerFactory`.
        let created = unsafe {
            CoCreateInstance(
                &CLSID_SPELL_CHECKER_FACTORY,
                std::ptr::null_mut(),
                CLSCTX_INPROC_SERVER,
                &IID_SPELL_CHECKER_FACTORY,
                &raw mut factory,
            )
        };
        if created < 0 {
            return None;
        }
        let factory = Com::<FactoryVtbl>::from_raw(factory)?;
        let languages = user_language().into_iter().chain(Some("en-US".to_owned()));
        for language in languages {
            let tag = wide(&language);
            let mut supported = 0;
            // SAFETY: a valid NUL-terminated tag and out pointer.
            let asked = unsafe {
                (factory.vtbl().is_supported)(factory.this(), tag.as_ptr(), &raw mut supported)
            };
            if asked < 0 || supported == 0 {
                continue;
            }
            let mut checker: This = std::ptr::null_mut();
            // SAFETY: as above; on success `checker` holds one reference to
            // an `ISpellChecker`.
            let made = unsafe {
                (factory.vtbl().create_spell_checker)(
                    factory.this(),
                    tag.as_ptr(),
                    &raw mut checker,
                )
            };
            if made >= 0
                && let Some(checker) = Com::from_raw(checker)
            {
                return Some(Self { checker });
            }
        }
        None
    }

    /// The strings of an `IEnumString`, at most `limit`.
    #[allow(unsafe_code)]
    fn strings(list: &Com<EnumStringVtbl>, limit: usize) -> Vec<String> {
        let mut out = Vec::new();
        while out.len() < limit {
            let mut item: *mut u16 = std::ptr::null_mut();
            let mut fetched = 0u32;
            // SAFETY: asks for one string into valid out pointers.
            let result =
                unsafe { (list.vtbl().next)(list.this(), 1, &raw mut item, &raw mut fetched) };
            if result != S_OK || fetched != 1 {
                break;
            }
            out.extend(take_string(item));
        }
        out
    }
}

impl Checker for WinSpellChecker {
    #[allow(unsafe_code)]
    fn check(&self, text: &[u16]) -> Vec<Found> {
        let mut found = Vec::new();
        if text.last() != Some(&0) {
            return found;
        }
        let mut errors: This = std::ptr::null_mut();
        // SAFETY: `text` is NUL-terminated (checked above); on success
        // `errors` holds one reference to an `IEnumSpellingError`.
        let checked = unsafe {
            (self.checker.vtbl().check)(self.checker.this(), text.as_ptr(), &raw mut errors)
        };
        if checked < 0 {
            return found;
        }
        let Some(errors) = Com::<EnumErrorsVtbl>::from_raw(errors) else {
            return found;
        };
        loop {
            let mut error: This = std::ptr::null_mut();
            // SAFETY: valid out pointer; `S_OK` hands out one reference.
            let next = unsafe { (errors.vtbl().next)(errors.this(), &raw mut error) };
            if next != S_OK {
                break;
            }
            let Some(error) = Com::<ErrorVtbl>::from_raw(error) else {
                break;
            };
            let (mut start, mut length, mut action) = (0u32, 0u32, 0i32);
            // SAFETY: valid out pointers for the error's fields.
            unsafe {
                (error.vtbl().start_index)(error.this(), &raw mut start);
                (error.vtbl().length)(error.this(), &raw mut length);
                (error.vtbl().corrective_action)(error.this(), &raw mut action);
            }
            found.push(Found {
                start: start as usize,
                len: length as usize,
                repeated: action == CORRECTIVE_ACTION_DELETE,
            });
        }
        found
    }

    #[allow(unsafe_code)]
    fn suggest(&self, word: &str) -> Vec<String> {
        let word = wide(word);
        let mut list: This = std::ptr::null_mut();
        // SAFETY: a NUL-terminated word and a valid out pointer; on success
        // `list` holds one reference to an `IEnumString`.
        let asked = unsafe {
            (self.checker.vtbl().suggest)(self.checker.this(), word.as_ptr(), &raw mut list)
        };
        if asked < 0 {
            return Vec::new();
        }
        Com::<EnumStringVtbl>::from_raw(list)
            .map_or_else(Vec::new, |list| Self::strings(&list, MAX_SUGGESTIONS))
    }

    #[allow(unsafe_code)]
    fn add(&self, word: &str) {
        let word = wide(word);
        // SAFETY: a NUL-terminated word.
        let _ = unsafe { (self.checker.vtbl().add)(self.checker.this(), word.as_ptr()) };
    }

    #[allow(unsafe_code)]
    fn ignore(&self, word: &str) {
        let word = wide(word);
        // SAFETY: a NUL-terminated word.
        let _ = unsafe { (self.checker.vtbl().ignore)(self.checker.this(), word.as_ptr()) };
    }
}
