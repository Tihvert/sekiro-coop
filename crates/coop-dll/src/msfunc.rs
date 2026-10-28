//! MSVC `std::function` objects implemented in Rust (FFI primitives for the native Co-op Settings page).
//!
//! Layout (x64 MSVC): 64 bytes, 56 bytes of inline storage, the impl pointer at +56. An impl is an
//! object whose first 8 bytes are a vtable pointer. Ours is `{ vtbl, row_id }`, stored INLINE at
//! `storage[0..16]`, with the impl pointer (= the storage address) at +56. Because it lives inside the
//! function object, `_Delete_this` has nothing to free.
//!
//! Vtable slots: 0 `_Copy(this, where) -> impl`, 1 `_Move(this, where) -> impl`, 2 `_Do_call`,
//! 3 `_Target_type`, 4 `_Delete_this(this, bool)`, 5 `_Get`.
//! Every body runs in `catch_unwind`; a panic returns 0 / `ret` and latches [`FAILED`].
#![allow(dead_code)]

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Our impl object. `vtbl` MUST stay the first field.
#[repr(C)]
pub struct FnImpl {
    pub vtbl: *const FnVtbl,
    pub row_id: u64,
}

/// The six MSVC `_Func_base` slots. `do_call` has a different signature per [`FnKind`], so it is a
/// plain code pointer here.
#[repr(C)]
pub struct FnVtbl {
    pub copy: unsafe extern "C" fn(*const FnImpl, *mut u8) -> *mut FnImpl,
    pub move_: unsafe extern "C" fn(*mut FnImpl, *mut u8) -> *mut FnImpl,
    pub do_call: *const (),
    pub target_type: unsafe extern "C" fn(*const FnImpl) -> *const u8,
    pub delete_this: unsafe extern "C" fn(*mut FnImpl, u8),
    pub get: unsafe extern "C" fn(*mut FnImpl) -> *mut u8,
}
unsafe impl Sync for FnVtbl {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FnKind {
    /// `std::function<uint8_t()>`
    Getter,
    /// `std::function<void(uint8_t)>`
    Setter,
    /// `std::function<ComboItemList<uint8_t>()>`
    List,
}

/// Set when any callback panicked. The page code checks it and falls back to the native page.
pub static FAILED: AtomicBool = AtomicBool::new(false);

pub fn latch_failed() {
    FAILED.store(true, Ordering::SeqCst);
}

pub fn failed() -> bool {
    FAILED.load(Ordering::SeqCst)
}

type GetFn = fn(u64) -> u8;
type SetFn = fn(u64, u8);
type ListFn = unsafe fn(u64, *mut u8) -> *mut u8;

#[derive(Clone, Copy)]
struct Handlers {
    get: GetFn,
    set: SetFn,
    list: ListFn,
}

static HANDLERS: Mutex<Option<Handlers>> = Mutex::new(None);

/// Registers the handlers the vtable bodies call. `get(row)` -> current index; `set(row, index)`;
/// `list(row, ret)` constructs the game's `ComboItemList<uint8_t>` in `ret` and returns `ret`.
pub fn set_handlers(get: fn(u64) -> u8, set: fn(u64, u8), list: unsafe fn(u64, *mut u8) -> *mut u8) {
    let mut g = HANDLERS.lock().unwrap_or_else(|e| e.into_inner());
    *g = Some(Handlers { get, set, list });
}

fn handlers() -> Option<Handlers> {
    *HANDLERS.lock().unwrap_or_else(|e| e.into_inner())
}

unsafe extern "C" fn v_copy(this: *const FnImpl, wh: *mut u8) -> *mut FnImpl {
    let r = catch_unwind(AssertUnwindSafe(|| unsafe {
        let src = &*this;
        let dst = wh as *mut FnImpl;
        std::ptr::write(dst, FnImpl { vtbl: src.vtbl, row_id: src.row_id });
        dst
    }));
    r.unwrap_or_else(|_| {
        latch_failed();
        wh as *mut FnImpl
    })
}

unsafe extern "C" fn v_move(this: *mut FnImpl, wh: *mut u8) -> *mut FnImpl {
    unsafe { v_copy(this, wh) }
}

unsafe extern "C" fn v_target_type(_this: *const FnImpl) -> *const u8 {
    std::ptr::null()
}

unsafe extern "C" fn v_delete(_this: *mut FnImpl, _del: u8) {}

unsafe extern "C" fn v_get(this: *mut FnImpl) -> *mut u8 {
    (this as *mut u8).wrapping_add(8)
}

unsafe extern "C" fn call_get(this: *const FnImpl) -> u8 {
    let r = catch_unwind(AssertUnwindSafe(|| {
        let row = unsafe { (*this).row_id };
        handlers().map(|h| (h.get)(row)).unwrap_or(0)
    }));
    r.unwrap_or_else(|_| {
        latch_failed();
        0
    })
}

unsafe extern "C" fn call_set(this: *const FnImpl, arg: *const u8) {
    let r = catch_unwind(AssertUnwindSafe(|| {
        let row = unsafe { (*this).row_id };
        let v = unsafe { *arg };
        if let Some(h) = handlers() {
            (h.set)(row, v);
        }
    }));
    if r.is_err() {
        latch_failed();
    }
}

unsafe extern "C" fn call_list(this: *const FnImpl, ret: *mut u8) -> *mut u8 {
    let r = catch_unwind(AssertUnwindSafe(|| {
        let row = unsafe { (*this).row_id };
        match handlers() {
            Some(h) => unsafe { (h.list)(row, ret) },
            None => ret,
        }
    }));
    r.unwrap_or_else(|_| {
        latch_failed();
        ret
    })
}

pub static VTBL_GETTER: FnVtbl = FnVtbl {
    copy: v_copy,
    move_: v_move,
    do_call: call_get as unsafe extern "C" fn(*const FnImpl) -> u8 as *const (),
    target_type: v_target_type,
    delete_this: v_delete,
    get: v_get,
};

pub static VTBL_SETTER: FnVtbl = FnVtbl {
    copy: v_copy,
    move_: v_move,
    do_call: call_set as unsafe extern "C" fn(*const FnImpl, *const u8) as *const (),
    target_type: v_target_type,
    delete_this: v_delete,
    get: v_get,
};

pub static VTBL_LIST: FnVtbl = FnVtbl {
    copy: v_copy,
    move_: v_move,
    do_call: call_list as unsafe extern "C" fn(*const FnImpl, *mut u8) -> *mut u8 as *const (),
    target_type: v_target_type,
    delete_this: v_delete,
    get: v_get,
};

fn vtbl_for(kind: FnKind) -> &'static FnVtbl {
    match kind {
        FnKind::Getter => &VTBL_GETTER,
        FnKind::Setter => &VTBL_SETTER,
        FnKind::List => &VTBL_LIST,
    }
}

/// Builds a `std::function` of `kind` for `row_id` in `storage` (64 bytes, any alignment >= 8).
/// The impl is written inline at `storage[0..16]`, the impl pointer (= `storage`) at `storage+56`,
/// the rest is zeroed.
///
/// # Safety
/// `storage` must point to 64 writable bytes aligned to 8.
pub unsafe fn init_function(storage: *mut u8, kind: FnKind, row_id: u64) {
    unsafe {
        std::ptr::write_bytes(storage, 0, 64);
        std::ptr::write(storage as *mut FnImpl, FnImpl { vtbl: vtbl_for(kind), row_id });
        std::ptr::write(storage.add(56) as *mut usize, storage as usize);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, AtomicU8};

    // Tests share HANDLERS / FAILED: serialise them.
    static LOCK: Mutex<()> = Mutex::new(());
    static LAST_SET: AtomicU64 = AtomicU64::new(0);
    static SET_VAL: AtomicU8 = AtomicU8::new(0);

    #[repr(C, align(8))]
    struct Buf([u8; 64]);

    fn h_get(row: u64) -> u8 {
        (row as u8).wrapping_add(1)
    }
    fn h_set(row: u64, v: u8) {
        LAST_SET.store(row, Ordering::SeqCst);
        SET_VAL.store(v, Ordering::SeqCst);
    }
    unsafe fn h_list(row: u64, ret: *mut u8) -> *mut u8 {
        unsafe { *ret = row as u8 };
        ret
    }
    fn p_get(_: u64) -> u8 {
        panic!("boom")
    }
    fn p_set(_: u64, _: u8) {
        panic!("boom")
    }
    unsafe fn p_list(_: u64, _: *mut u8) -> *mut u8 {
        panic!("boom")
    }

    unsafe fn vtbl_of(buf: *const u8) -> *const FnVtbl {
        unsafe {
            let imp = *(buf.add(56) as *const *const FnImpl);
            (*imp).vtbl
        }
    }

    unsafe fn call_g(buf: *const u8) -> u8 {
        unsafe {
            let imp = *(buf.add(56) as *const *const FnImpl);
            let f: unsafe extern "C" fn(*const FnImpl) -> u8 = std::mem::transmute((*vtbl_of(buf)).do_call);
            f(imp)
        }
    }

    #[test]
    fn layout_and_copy_move() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_handlers(h_get, h_set, h_list);
        let mut a = Buf([0xAA; 64]);
        let mut b = Buf([0; 64]);
        let mut c = Buf([0; 64]);
        unsafe {
            init_function(a.0.as_mut_ptr(), FnKind::Getter, 7);
            assert_eq!(*(a.0.as_ptr().add(56) as *const usize), a.0.as_ptr() as usize);
            assert!(a.0[16..56].iter().all(|&x| x == 0));
            let vt = vtbl_of(a.0.as_ptr());
            let imp = *(a.0.as_ptr().add(56) as *const *const FnImpl);
            let r = ((*vt).copy)(imp, b.0.as_mut_ptr());
            assert_eq!(r as usize, b.0.as_ptr() as usize);
            assert_eq!(&b.0[0..16], &a.0[0..16]);
            // finish the copy like MSVC does: impl pointer at +56 of the new object
            *(b.0.as_mut_ptr().add(56) as *mut usize) = r as usize;
            assert_eq!(call_g(b.0.as_ptr()), 8);
            let r2 = ((*vtbl_of(b.0.as_ptr())).move_)(r, c.0.as_mut_ptr());
            assert_eq!(r2 as usize, c.0.as_ptr() as usize);
            assert_eq!(&c.0[0..16], &a.0[0..16]);
            *(c.0.as_mut_ptr().add(56) as *mut usize) = r2 as usize;
            assert_eq!(call_g(c.0.as_ptr()), 8);
            // _Get, _Target_type, _Delete_this
            assert_eq!(((*vt).get)(imp as *mut FnImpl) as usize, imp as usize + 8);
            assert!(((*vt).target_type)(imp).is_null());
            ((*vt).delete_this)(imp as *mut FnImpl, 0);
            assert_eq!(call_g(a.0.as_ptr()), 8);
        }
        assert!(!failed());
    }

    #[test]
    fn setter_and_list() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_handlers(h_get, h_set, h_list);
        let mut s = Buf([0; 64]);
        let mut l = Buf([0; 64]);
        unsafe {
            init_function(s.0.as_mut_ptr(), FnKind::Setter, 3);
            let imp = *(s.0.as_ptr().add(56) as *const *const FnImpl);
            let f: unsafe extern "C" fn(*const FnImpl, *const u8) = std::mem::transmute((*vtbl_of(s.0.as_ptr())).do_call);
            let v = 9u8;
            f(imp, &v);
            assert_eq!(LAST_SET.load(Ordering::SeqCst), 3);
            assert_eq!(SET_VAL.load(Ordering::SeqCst), 9);

            init_function(l.0.as_mut_ptr(), FnKind::List, 42);
            let imp = *(l.0.as_ptr().add(56) as *const *const FnImpl);
            let g: unsafe extern "C" fn(*const FnImpl, *mut u8) -> *mut u8 = std::mem::transmute((*vtbl_of(l.0.as_ptr())).do_call);
            let mut ret = [0u8; 8];
            let p = g(imp, ret.as_mut_ptr());
            assert_eq!(p, ret.as_mut_ptr());
            assert_eq!(ret[0], 42);
        }
    }

    #[test]
    fn panic_returns_default_and_latches() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_handlers(p_get, p_set, p_list);
        FAILED.store(false, Ordering::SeqCst);
        let mut a = Buf([0; 64]);
        unsafe {
            init_function(a.0.as_mut_ptr(), FnKind::Getter, 1);
            assert_eq!(call_g(a.0.as_ptr()), 0);
            assert!(failed());
            FAILED.store(false, Ordering::SeqCst);
            init_function(a.0.as_mut_ptr(), FnKind::Setter, 1);
            let imp = *(a.0.as_ptr().add(56) as *const *const FnImpl);
            let f: unsafe extern "C" fn(*const FnImpl, *const u8) = std::mem::transmute((*vtbl_of(a.0.as_ptr())).do_call);
            f(imp, &1u8);
            assert!(failed());
            FAILED.store(false, Ordering::SeqCst);
            init_function(a.0.as_mut_ptr(), FnKind::List, 1);
            let imp = *(a.0.as_ptr().add(56) as *const *const FnImpl);
            let g: unsafe extern "C" fn(*const FnImpl, *mut u8) -> *mut u8 = std::mem::transmute((*vtbl_of(a.0.as_ptr())).do_call);
            let mut ret = [0u8; 8];
            assert_eq!(g(imp, ret.as_mut_ptr()), ret.as_mut_ptr());
            assert!(failed());
        }
        FAILED.store(false, Ordering::SeqCst);
        set_handlers(h_get, h_set, h_list);
    }
}
