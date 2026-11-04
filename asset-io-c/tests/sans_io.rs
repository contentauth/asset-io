use std::{ffi::CString, ptr};

use asset_io_c::*;

struct Handle<T>(*mut T);
impl<T> Drop for Handle<T> {
    fn drop(&mut self) {
        assert_eq!(asset_io_free(self.0.cast()), 0);
    }
}
fn owned<T>(p: *mut T) -> Handle<T> {
    assert!(!p.is_null(), "{}", error());
    Handle(p)
}
fn bytes(p: *mut AssetIoBuffer) -> Vec<u8> {
    let mut len = 0;
    let data = asset_io_buffer_data(p, &mut len);
    assert!(!data.is_null());
    unsafe { std::slice::from_raw_parts(data, len).to_vec() }
}
fn error() -> String {
    let p = asset_io_last_error();
    assert!(!p.is_null(), "expected an error");
    let e = Handle(p);
    String::from_utf8(bytes(e.0)).unwrap()
}
fn registry() -> Handle<AssetIoRegistry> {
    owned(asset_io_registry_new(1))
}

#[test]
fn sans_io_matches_blocking_and_owns_inputs_and_result() {
    let inputs: Vec<&[u8]> = vec![
        #[cfg(feature = "jpeg")]
        include_bytes!("../../tests/fixtures/Designer.jpeg"),
        #[cfg(feature = "png")]
        include_bytes!("../../tests/fixtures/GreenCat.png"),
    ];
    for input in inputs {
        for explicit in [false, true] {
            let r = registry();
            let format = CString::new(if input.starts_with(b"\xff\xd8") {
                "jpg"
            } else {
                "png"
            })
            .unwrap();
            let format_ptr = if explicit {
                format.as_ptr()
            } else {
                ptr::null()
            };
            let blocking = owned(asset_io_parse_memory(
                r.0,
                format_ptr,
                input.as_ptr(),
                input.len(),
            ));
            let expected_structure = owned(asset_io_asset_structure(blocking.0));
            let expected = owned(asset_io_structure_json(expected_structure.0));
            let task = owned(asset_io_parse_task_new(r.0, format_ptr, input.len() as u64));
            drop(format);
            drop(r);
            let mut request = AssetIoReadRequest::default();
            let mut reads = 0;
            loop {
                match asset_io_parse_task_resume(task.0, &mut request) {
                    1 => {
                        reads += 1;
                        let at = usize::try_from(request.offset).unwrap();
                        assert!(request.min_len > 0);
                        assert!(request.min_len <= request.window_len);
                        assert!(at + request.window_len <= input.len());
                        let mut response = input[at..at + request.window_len].to_vec();
                        assert_eq!(
                            asset_io_parse_task_supply(task.0, response.as_ptr(), response.len()),
                            0
                        );
                        response.fill(0); // Supplying copies; no caller buffer is retained.
                    }
                    0 => break,
                    _ => panic!("{}", error()),
                }
            }
            assert!(reads > 1, "exercise multiple suspensions");
            assert_eq!(
                (request.offset, request.min_len, request.window_len),
                (0, 0, 0)
            );
            assert_eq!(asset_io_parse_task_resume(task.0, &mut request), 0);
            let structure = owned(asset_io_parse_task_structure(task.0));
            drop(task);
            let actual = owned(asset_io_structure_json(structure.0));
            assert_eq!(bytes(actual.0), bytes(expected.0));
        }
    }
}

#[test]
fn sans_io_checks_handles_and_deferred_format_errors() {
    let r = registry();
    let mut request = AssetIoReadRequest::default();
    assert!(asset_io_parse_task_new(ptr::null_mut(), ptr::null(), 4).is_null());
    let invalid = [255u8, 0];
    assert!(asset_io_parse_task_new(r.0, invalid.as_ptr().cast(), 4).is_null());
    let format = CString::new("not-a-format").unwrap();
    let task = owned(asset_io_parse_task_new(r.0, format.as_ptr(), 4));
    assert_eq!(asset_io_parse_task_resume(task.0, ptr::null_mut()), -1);
    assert_eq!(asset_io_parse_task_supply(task.0, ptr::null(), 0), -1);
    assert!(asset_io_parse_task_structure(task.0).is_null());
    assert_eq!(asset_io_parse_task_resume(task.0, &mut request), -1);
    assert!(error().starts_with("UnsupportedFormat:"));
    assert_eq!(asset_io_parse_task_resume(task.0, &mut request), -1);
    assert!(error().contains("UnsupportedFormat"));
    assert_eq!(asset_io_parse_task_resume(r.0.cast(), &mut request), -1);
    let stale = asset_io_parse_task_new(r.0, ptr::null(), 4);
    assert!(!stale.is_null());
    assert_eq!(asset_io_free(stale.cast()), 0); // Cancel before starting.
    assert_eq!(asset_io_parse_task_resume(stale, &mut request), -1);
}

#[cfg(feature = "jpeg")]
#[test]
fn sans_io_waiting_errors_and_cancellation() {
    let r = registry();
    let jpeg = b"\xff\xd8\xff\xd9";
    let mut request = AssetIoReadRequest::default();
    let task = owned(asset_io_parse_task_new(r.0, ptr::null(), 4));
    assert_eq!(asset_io_parse_task_resume(task.0, &mut request), 1);
    let first = (request.offset, request.min_len, request.window_len);
    assert_eq!(asset_io_parse_task_resume(task.0, &mut request), 1);
    assert_eq!((request.offset, request.min_len, request.window_len), first);
    assert_eq!(asset_io_parse_task_supply(task.0, ptr::null(), 1), -1);
    assert_eq!(asset_io_parse_task_supply(task.0, jpeg.as_ptr(), 5), -1);
    assert_eq!(asset_io_parse_task_resume(task.0, &mut request), 1);
    assert_eq!(asset_io_parse_task_supply(task.0, jpeg.as_ptr(), 4), 0);
    assert_eq!(asset_io_parse_task_supply(task.0, jpeg.as_ptr(), 4), -1);
    loop {
        let status = asset_io_parse_task_resume(task.0, &mut request);
        if status == 0 {
            break;
        }
        assert_eq!(status, 1, "{}", error());
        assert_eq!(asset_io_parse_task_supply(task.0, jpeg.as_ptr(), 4), 0);
    }
    let _structure = owned(asset_io_parse_task_structure(task.0));
    // Cancel a suspended task and reject late host completions.
    let cancelled = asset_io_parse_task_new(r.0, ptr::null(), 4);
    assert_eq!(asset_io_parse_task_resume(cancelled, &mut request), 1);
    assert_eq!(asset_io_free(cancelled.cast()), 0);
    assert_eq!(asset_io_parse_task_supply(cancelled, jpeg.as_ptr(), 4), -1);

    let short = owned(asset_io_parse_task_new(r.0, ptr::null(), 4));
    assert_eq!(asset_io_parse_task_resume(short.0, &mut request), 1);
    assert_eq!(asset_io_parse_task_supply(short.0, ptr::null(), 0), 0);
    assert_eq!(asset_io_parse_task_resume(short.0, &mut request), -1);
    assert!(error().starts_with("Io:"));
    assert!(asset_io_parse_task_structure(short.0).is_null());

    let failed = owned(asset_io_parse_task_new(r.0, ptr::null(), 4));
    let message = CString::new("native pread failed").unwrap();
    assert_eq!(asset_io_parse_task_fail(failed.0, message.as_ptr()), -1);
    assert_eq!(asset_io_parse_task_resume(failed.0, &mut request), 1);
    assert_eq!(asset_io_parse_task_fail(failed.0, ptr::null()), -1);
    assert_eq!(asset_io_parse_task_fail(failed.0, message.as_ptr()), 0);
    drop(message);
    assert_eq!(asset_io_parse_task_resume(failed.0, &mut request), -1);
    assert!(error().contains("native pread failed"));
    assert_eq!(asset_io_parse_task_supply(failed.0, jpeg.as_ptr(), 4), -1);
}
