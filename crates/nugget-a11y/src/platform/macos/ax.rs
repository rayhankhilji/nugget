//! Safe wrappers over the few ApplicationServices (HIServices) AX calls the backend needs, and
//! the one CGEvent call that takes a raw buffer. All of this crate's macOS `unsafe` lives here:
//! the functions are `unsafe` only because they take out-pointers or buffers, which these
//! wrappers own.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_application_services::{AXError, AXUIElement, AXValue, AXValueType};
use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFIndex, CFRange, CFRetained, CFString, CFType, CGPoint,
    CGRect, CGSize,
};
use objc2_core_graphics::CGEvent;

pub type Ax = CFRetained<AXUIElement>;

/// The system-wide element, with a messaging timeout so a hung app cannot hang us.
pub fn system_wide() -> Ax {
    // SAFETY: AXUIElementCreateSystemWide has no preconditions and returns a +1 reference.
    let sys = unsafe { AXUIElement::new_system_wide() };
    // SAFETY: `sys` is a valid element; the timeout applies to every element in this process.
    let _ = unsafe { sys.set_messaging_timeout(1.0) };
    sys
}

/// `AXIsProcessTrustedWithOptions`, optionally showing the system prompt.
pub fn is_trusted(prompt: bool) -> bool {
    // SAFETY: kAXTrustedCheckOptionPrompt is an immutable CFString constant exported by
    // HIServices, valid for the life of the process.
    let key: &CFString = unsafe { objc2_application_services::kAXTrustedCheckOptionPrompt };
    let value = CFBoolean::new(prompt);
    let dict = CFDictionary::<CFString, CFBoolean>::from_slices(&[key], &[value]);
    // SAFETY: the options dictionary maps the documented key to a CFBoolean.
    unsafe { objc2_application_services::AXIsProcessTrustedWithOptions(Some(dict.as_opaque())) }
}

/// Copy an attribute value (`None` on any AX error, including "no value").
pub fn attr(el: &AXUIElement, name: &str) -> Option<CFRetained<CFType>> {
    let name = CFString::from_str(name);
    let mut out: *const CFType = std::ptr::null();
    // SAFETY: `out` is a valid out-pointer; on success it receives a +1 reference we own.
    let err = unsafe { el.copy_attribute_value(&name, NonNull::from(&mut out)) };
    if err != AXError::Success {
        return None;
    }
    // SAFETY: on success `out` is a retained, non-null CFTypeRef (checked by NonNull::new).
    NonNull::new(out.cast_mut()).map(|p| unsafe { CFRetained::from_raw(p) })
}

/// Copy up to `max` values of an array attribute (`AXChildren`) without copying the rest.
pub fn attr_values(el: &AXUIElement, name: &str, max: usize) -> Vec<CFRetained<CFType>> {
    let name = CFString::from_str(name);
    let mut out: *const CFArray = std::ptr::null();
    let max = CFIndex::try_from(max).unwrap_or(CFIndex::MAX);
    // SAFETY: `out` is a valid out-pointer; on success it receives a +1 CFArray we own.
    let err = unsafe { el.copy_attribute_values(&name, 0, max, NonNull::from(&mut out)) };
    if err != AXError::Success {
        return Vec::new();
    }
    let Some(p) = NonNull::new(out.cast_mut()) else {
        return Vec::new();
    };
    // SAFETY: a retained CFArray from AXUIElementCopyAttributeValues.
    let arr: CFRetained<CFArray> = unsafe { CFRetained::from_raw(p) };
    // SAFETY: the array holds CFTypeRefs (AX returns CF objects only).
    let arr: CFRetained<CFArray<CFType>> = unsafe { CFRetained::cast_unchecked(arr) };
    arr.to_vec()
}

/// Read several attributes in one round trip. Missing ones come back as `None`.
pub fn attrs(el: &AXUIElement, names: &[&str]) -> Vec<Option<CFRetained<CFType>>> {
    let keys: Vec<CFRetained<CFString>> = names.iter().map(|n| CFString::from_str(n)).collect();
    let refs: Vec<&CFString> = keys.iter().map(|k| &**k).collect();
    let arr = CFArray::<CFString>::from_objects(&refs);
    let mut out: *const CFArray = std::ptr::null();
    // SAFETY: `arr` is a CFArray of CFStrings; `out` is a valid out-pointer that receives a
    // +1 CFArray on success. Options 0: values for missing attributes are AXValue errors.
    let err = unsafe {
        el.copy_multiple_attribute_values(
            arr.as_opaque(),
            objc2_application_services::AXCopyMultipleAttributeOptions(0),
            NonNull::from(&mut out),
        )
    };
    let none = || names.iter().map(|_| None).collect();
    if err != AXError::Success {
        return none();
    }
    let Some(p) = NonNull::new(out.cast_mut()) else {
        return none();
    };
    // SAFETY: a retained CFArray of CFTypeRefs, one per requested attribute.
    let arr: CFRetained<CFArray> = unsafe { CFRetained::from_raw(p) };
    // SAFETY: as above.
    let arr: CFRetained<CFArray<CFType>> = unsafe { CFRetained::cast_unchecked(arr) };
    let mut v: Vec<Option<CFRetained<CFType>>> = arr
        .to_vec()
        .into_iter()
        .map(|t| {
            // Missing attributes are AXValues of type kAXValueAXErrorType.
            match t.downcast_ref::<AXValue>() {
                // SAFETY: `t` is a valid AXValue.
                Some(v) if unsafe { v.r#type() } == AXValueType::AXError => None,
                _ => Some(t),
            }
        })
        .collect();
    v.resize_with(names.len(), || None);
    v
}

pub fn is_settable(el: &AXUIElement, name: &str) -> bool {
    let name = CFString::from_str(name);
    let mut settable: u8 = 0;
    // SAFETY: `settable` is a valid out-pointer to a Boolean.
    let err = unsafe { el.is_attribute_settable(&name, NonNull::from(&mut settable).cast()) };
    err == AXError::Success && settable != 0
}

pub fn set_attr(el: &AXUIElement, name: &str, value: &CFType) -> Result<(), AXError> {
    let name = CFString::from_str(name);
    // SAFETY: `el` is a valid element, `value` a valid CF object of the attribute's type.
    let err = unsafe { el.set_attribute_value(&name, value) };
    if err == AXError::Success {
        Ok(())
    } else {
        Err(err)
    }
}

pub fn param_attr(el: &AXUIElement, name: &str, param: &CFType) -> Option<CFRetained<CFType>> {
    let name = CFString::from_str(name);
    let mut out: *const CFType = std::ptr::null();
    // SAFETY: `param` is a valid CF object; `out` receives a +1 reference on success.
    let err =
        unsafe { el.copy_parameterized_attribute_value(&name, param, NonNull::from(&mut out)) };
    if err != AXError::Success {
        return None;
    }
    // SAFETY: retained and non-null on success.
    NonNull::new(out.cast_mut()).map(|p| unsafe { CFRetained::from_raw(p) })
}

pub fn pid(el: &AXUIElement) -> Option<i32> {
    let mut pid: i32 = 0;
    // SAFETY: `pid` is a valid out-pointer.
    let err = unsafe { el.pid(NonNull::from(&mut pid)) };
    (err == AXError::Success).then_some(pid)
}

fn value_of<T: Default>(v: &AXValue, ty: AXValueType) -> Option<T> {
    let mut out = T::default();
    // SAFETY: `out` is a valid pointer to a value of the C struct matching `ty` (checked by
    // AXValueGetValue, which returns false on a type mismatch).
    let ok = unsafe { v.value(ty, NonNull::from(&mut out).cast::<c_void>()) };
    ok.then_some(out)
}

pub fn as_point(t: &CFType) -> Option<CGPoint> {
    value_of(t.downcast_ref::<AXValue>()?, AXValueType::CGPoint)
}

pub fn as_size(t: &CFType) -> Option<CGSize> {
    value_of(t.downcast_ref::<AXValue>()?, AXValueType::CGSize)
}

pub fn as_rect(t: &CFType) -> Option<CGRect> {
    value_of(t.downcast_ref::<AXValue>()?, AXValueType::CGRect)
}

pub fn as_range(t: &CFType) -> Option<CFRange> {
    let v = t.downcast_ref::<AXValue>()?;
    let mut out = CFRange {
        location: 0,
        length: 0,
    };
    // SAFETY: `out` is a valid CFRange; AXValueGetValue checks the type.
    let ok = unsafe {
        v.value(
            AXValueType::CFRange,
            NonNull::from(&mut out).cast::<c_void>(),
        )
    };
    ok.then_some(out)
}

pub fn range_value(location: usize, length: usize) -> Option<CFRetained<AXValue>> {
    let mut r = CFRange {
        location: CFIndex::try_from(location).ok()?,
        length: CFIndex::try_from(length).ok()?,
    };
    // SAFETY: `r` is a valid CFRange for the duration of the call; AXValueCreate copies it.
    unsafe { AXValue::new(AXValueType::CFRange, NonNull::from(&mut r).cast::<c_void>()) }
}

/// The first `max` elements of a CFArray value (e.g. `AXDOMClassList`), empty for non-arrays.
pub fn array_items(t: &CFType, max: usize) -> Vec<CFRetained<CFType>> {
    let Some(arr) = t.downcast_ref::<CFArray>() else {
        return Vec::new();
    };
    let ptr: *const CFArray = arr;
    // SAFETY: `CFArray<T>` is a marker view of the same opaque CFArray; AX arrays hold
    // CFTypeRefs, which `get` retains and callers downcast with a type check.
    let typed: &CFArray<CFType> = unsafe { &*ptr.cast::<CFArray<CFType>>() };
    (0..typed.len().min(max))
        .filter_map(|i| typed.get(i))
        .collect()
}

/// The application element of a process (`AXUIElementCreateApplication`).
pub fn application(pid: i32) -> Ax {
    // SAFETY: no preconditions; an unknown pid yields an element whose calls fail.
    unsafe { AXUIElement::new_application(pid) }
}

/// The element's action names (`AXPress`, `AXShowMenu`...), up to 32.
pub fn action_names(el: &AXUIElement) -> Vec<String> {
    let mut out: *const CFArray = std::ptr::null();
    // SAFETY: `out` is a valid out-pointer; on success it receives a +1 CFArray we own.
    let err = unsafe { el.copy_action_names(NonNull::from(&mut out)) };
    if err != AXError::Success {
        return Vec::new();
    }
    let Some(p) = NonNull::new(out.cast_mut()) else {
        return Vec::new();
    };
    // SAFETY: a retained CFArray from AXUIElementCopyActionNames.
    let arr: CFRetained<CFArray> = unsafe { CFRetained::from_raw(p) };
    array_items(&arr, 32)
        .iter()
        .filter_map(|t| t.downcast_ref::<CFString>().map(|s| s.to_string()))
        .collect()
}

pub fn perform_action(el: &AXUIElement, action: &str) -> Result<(), AXError> {
    let action = CFString::from_str(action);
    // SAFETY: `el` is a valid element and `action` a valid CFString.
    let err = unsafe { el.perform_action(&action) };
    if err == AXError::Success {
        Ok(())
    } else {
        Err(err)
    }
}

/// A value of a CFDictionary with string keys (`CGWindowListCopyWindowInfo` entries).
pub fn dict_get(t: &CFType, key: &str) -> Option<CFRetained<CFType>> {
    let dict = t.downcast_ref::<CFDictionary>()?;
    let ptr: *const CFDictionary = dict;
    // SAFETY: `CFDictionary<K, V>` is a marker view of the same opaque CFDictionary; the window
    // server's dictionaries have CFString keys and CF object values, and the value is retained
    // by `get` and downcast with a type check by callers.
    let typed: &CFDictionary<CFString, CFType> =
        unsafe { &*ptr.cast::<CFDictionary<CFString, CFType>>() };
    typed.get(&CFString::from_str(key))
}

/// Attach UTF-16 text to a keyboard event (`CGEventKeyboardSetUnicodeString`). The system
/// reads at most 20 units, so callers chunk longer text.
pub fn set_unicode(ev: &CGEvent, units: &[u16]) {
    // SAFETY: `units` is a valid buffer of `units.len()` UTF-16 code units for the call; the
    // event copies them.
    unsafe { CGEvent::keyboard_set_unicode_string(Some(ev), units.len() as _, units.as_ptr()) }
}
