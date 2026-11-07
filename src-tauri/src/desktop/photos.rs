//! Selected image files stay in native staging until the host validates and
//! publishes them. ImageIO applies orientation while decoding a bounded JPEG.
use super::picker::{self, Operation};
use core_foundation::{
  base::{kCFAllocatorDefault, CFType, CFTypeRef, TCFType},
  boolean::CFBoolean,
  data::{CFData, CFDataCreateMutable, CFMutableDataRef},
  dictionary::{CFDictionary, CFDictionaryRef},
  number::CFNumber,
  string::{CFString, CFStringRef},
  url::{CFURLRef, CFURL},
};
use core_graphics::sys::CGImageRef;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
  ffi::c_void,
  fs::{self, OpenOptions},
  io::Write,
  path::{Path, PathBuf},
  ptr,
};
use tauri::{AppHandle, Runtime};

const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_PIXEL_SIZE: usize = 4096;
const EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "gif", "webp", "heic", "heif", "avif"];

#[derive(Deserialize)]
struct Request {
  kind: String,
  session_id: String,
  operation_id: String,
  staging_path: PathBuf,
  max_image_bytes: u64,
  output_format: Option<String>,
}

pub fn run<R: Runtime>(app: &AppHandle<R>, args: Value) -> Result<Value, String> {
  let request: Request =
    serde_json::from_value(args).map_err(|_| "Invalid photo import request.")?;
  if request.kind != "photos"
    || !request.staging_path.is_absolute()
    || !(1..=MAX_IMAGE_BYTES).contains(&request.max_image_bytes)
    || request
      .output_format
      .as_deref()
      .is_some_and(|format| format != "jpeg")
  {
    return Err("Invalid photo import request.".into());
  }
  let operation = Operation::begin(&request.session_id, &request.operation_id)?;
  let mut stored = false;
  let result = (|| {
    let Some(source) = picker::choose(app, &operation, "Choose a photo", EXTENSIONS)? else {
      return Ok(json!({"status":"cancelled"}));
    };
    stage_photo(
      &source,
      &request.staging_path,
      request.max_image_bytes,
      request.output_format.is_some(),
      &operation,
    )?;
    stored = true;
    operation.ensure_active()?;
    Ok(json!({"status":"image","remember":false}))
  })();
  let cancelled = operation.cancelled();
  if stored && (cancelled || result.is_err()) {
    let _ = fs::remove_file(&request.staging_path);
  }
  if cancelled {
    Ok(json!({"status":"cancelled"}))
  } else {
    result
  }
}

fn stage_photo(
  source: &Path,
  destination: &Path,
  max_bytes: u64,
  jpeg: bool,
  operation: &Operation,
) -> Result<(), String> {
  if !jpeg {
    picker::copy_file(source, destination, max_bytes, operation)?;
    return Ok(());
  }
  let parent = destination
    .parent()
    .ok_or("Could not prepare image storage.")?;
  // Bound the source before ImageIO reads it. A private temporary copy prevents
  // changes to the selected file from racing the decoder's lazy reads.
  let temporary = tempfile::tempdir_in(parent).map_err(|_| "Could not prepare image storage.")?;
  let staged_source = temporary.path().join("source");
  picker::copy_file(source, &staged_source, max_bytes, operation)?;
  let data = jpeg_bytes(&staged_source, max_bytes, || operation.ensure_active())?;
  let mut output = OpenOptions::new()
    .write(true)
    .create_new(true)
    .open(destination)
    .map_err(|_| "Could not prepare the imported image.")?;
  let result = (|| {
    for bytes in data.bytes().chunks(64 * 1024) {
      operation.ensure_active()?;
      output
        .write_all(bytes)
        .map_err(|_| "Could not save the imported image.")?;
    }
    output
      .flush()
      .map_err(|_| "Could not finish importing the image.")?;
    operation.ensure_active()
  })();
  drop(output);
  if result.is_err() {
    let _ = fs::remove_file(destination);
  }
  result
}

fn cf_string(reference: CFStringRef) -> CFString {
  // These references are ImageIO's retained global property keys.
  unsafe { CFString::wrap_under_get_rule(reference) }
}

fn owned(reference: CFTypeRef, error: &'static str) -> Result<CFType, String> {
  if reference.is_null() {
    return Err(error.into());
  }
  // All callers supply a non-null object returned by a Create or Copy API.
  Ok(unsafe { CFType::wrap_under_create_rule(reference) })
}

fn image_source(path: &Path) -> Result<CFType, String> {
  let url = CFURL::from_path(path, false).ok_or("Could not read the selected image.")?;
  let options = unsafe {
    CFDictionary::from_CFType_pairs(&[(
      cf_string(kCGImageSourceShouldCache),
      CFBoolean::false_value().into_CFType(),
    )])
  };
  owned(
    unsafe { CGImageSourceCreateWithURL(url.as_concrete_TypeRef(), options.as_concrete_TypeRef()) },
    "The selected file could not be decoded as an image.",
  )
}

fn jpeg_bytes(
  source: &Path,
  max_bytes: u64,
  ensure_active: impl Fn() -> Result<(), String>,
) -> Result<CFData, String> {
  ensure_active()?;
  let source = image_source(source)?;
  let options = unsafe {
    CFDictionary::from_CFType_pairs(&[
      (
        cf_string(kCGImageSourceCreateThumbnailFromImageAlways),
        CFBoolean::true_value().into_CFType(),
      ),
      (
        cf_string(kCGImageSourceCreateThumbnailWithTransform),
        CFBoolean::true_value().into_CFType(),
      ),
      (
        cf_string(kCGImageSourceThumbnailMaxPixelSize),
        CFNumber::from(MAX_PIXEL_SIZE as i64).into_CFType(),
      ),
      (
        cf_string(kCGImageSourceShouldCacheImmediately),
        CFBoolean::true_value().into_CFType(),
      ),
    ])
  };
  ensure_active()?;
  let image = unsafe {
    CGImageSourceCreateThumbnailAtIndex(
      source.as_CFTypeRef().cast_mut(),
      0,
      options.as_concrete_TypeRef(),
    )
  };
  let image = owned(image.cast(), "The photo could not be converted to JPEG.")?;
  ensure_active()?;
  let raw_image = image.as_CFTypeRef().cast_mut().cast();
  let width = unsafe { CGImageGetWidth(raw_image) };
  let height = unsafe { CGImageGetHeight(raw_image) };
  if width == 0 || height == 0 || width.max(height) > MAX_PIXEL_SIZE {
    return Err("The photo has invalid dimensions.".into());
  }
  // Add raster pixels, rather than copying an ImageIO source. This deliberately
  // omits the original EXIF/GPS metadata and bakes its orientation into pixels.
  let data = unsafe { CFDataCreateMutable(kCFAllocatorDefault, 0) };
  if data.is_null() {
    return Err("Could not prepare image encoding.".into());
  }
  let data = unsafe { CFData::wrap_under_create_rule(data) };
  let jpeg = CFString::from_static_string("public.jpeg");
  let destination = owned(
    unsafe {
      CGImageDestinationCreateWithData(
        data.as_concrete_TypeRef().cast_mut(),
        jpeg.as_concrete_TypeRef(),
        1,
        ptr::null(),
      )
    },
    "Could not prepare JPEG encoding.",
  )?;
  let properties = unsafe {
    CFDictionary::from_CFType_pairs(&[(
      cf_string(kCGImageDestinationLossyCompressionQuality),
      CFNumber::from(0.9).into_CFType(),
    )])
  };
  ensure_active()?;
  unsafe {
    CGImageDestinationAddImage(
      destination.as_CFTypeRef().cast_mut(),
      raw_image,
      properties.as_concrete_TypeRef(),
    );
    if !CGImageDestinationFinalize(destination.as_CFTypeRef().cast_mut()) {
      return Err("The photo could not be encoded as JPEG.".into());
    }
  }
  ensure_active()?;
  if data.is_empty() || data.len() as u64 > max_bytes {
    return Err("The converted photo exceeds the image size limit.".into());
  }
  Ok(data)
}

type ImageSourceRef = *mut c_void;
type ImageDestinationRef = *mut c_void;

#[link(name = "ImageIO", kind = "framework")]
unsafe extern "C" {
  static kCGImageSourceShouldCache: CFStringRef;
  static kCGImageSourceShouldCacheImmediately: CFStringRef;
  static kCGImageSourceCreateThumbnailFromImageAlways: CFStringRef;
  static kCGImageSourceCreateThumbnailWithTransform: CFStringRef;
  static kCGImageSourceThumbnailMaxPixelSize: CFStringRef;
  static kCGImageDestinationLossyCompressionQuality: CFStringRef;
  fn CGImageSourceCreateWithURL(url: CFURLRef, options: CFDictionaryRef) -> ImageSourceRef;
  fn CGImageSourceCreateThumbnailAtIndex(
    source: ImageSourceRef,
    index: usize,
    options: CFDictionaryRef,
  ) -> CGImageRef;
  fn CGImageDestinationCreateWithData(
    data: CFMutableDataRef,
    kind: CFStringRef,
    count: usize,
    options: CFDictionaryRef,
  ) -> ImageDestinationRef;
  fn CGImageDestinationAddImage(
    destination: ImageDestinationRef,
    image: CGImageRef,
    properties: CFDictionaryRef,
  );
  fn CGImageDestinationFinalize(destination: ImageDestinationRef) -> bool;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
  fn CGImageGetWidth(image: CGImageRef) -> usize;
  fn CGImageGetHeight(image: CGImageRef) -> usize;
}

#[cfg(test)]
mod tests {
  use super::*;
  use core_graphics::{
    geometry::{CGPoint, CGRect, CGSize},
    sys::{CGColorSpaceRef, CGContextRef},
  };
  use std::cell::Cell;

  const COMMENT: &str = "Image conversion fixture";

  fn fixture(path: &Path) {
    unsafe {
      let colors = CGColorSpaceCreateDeviceRGB();
      assert!(!colors.is_null());
      // A thin large image exercises downsampling without a large allocation.
      let context = CGBitmapContextCreate(ptr::null_mut(), 5000, 20, 8, 0, colors, 5);
      CGColorSpaceRelease(colors);
      assert!(!context.is_null());
      CGContextSetRGBFillColor(context, 0.2, 0.7, 0.3, 1.0);
      CGContextFillRect(
        context,
        CGRect::new(&CGPoint::new(0., 0.), &CGSize::new(5000., 20.)),
      );
      let image = CGBitmapContextCreateImage(context);
      CGContextRelease(context);
      let image = owned(image.cast(), "Could not create test pixels.").unwrap();
      let url = CFURL::from_path(path, false).unwrap();
      let jpeg = CFString::from_static_string("public.jpeg");
      let destination = owned(
        CGImageDestinationCreateWithURL(
          url.as_concrete_TypeRef(),
          jpeg.as_concrete_TypeRef(),
          1,
          ptr::null(),
        ),
        "Could not create the test image.",
      )
      .unwrap();
      let exif = CFDictionary::from_CFType_pairs(&[(
        cf_string(kCGImagePropertyExifUserComment),
        CFString::new(COMMENT).into_CFType(),
      )]);
      let properties = CFDictionary::from_CFType_pairs(&[
        (
          cf_string(kCGImagePropertyOrientation),
          CFNumber::from(6).into_CFType(),
        ),
        (
          cf_string(kCGImagePropertyExifDictionary),
          exif.into_CFType(),
        ),
      ]);
      CGImageDestinationAddImage(
        destination.as_CFTypeRef().cast_mut(),
        image.as_CFTypeRef().cast_mut().cast(),
        properties.as_concrete_TypeRef(),
      );
      assert!(CGImageDestinationFinalize(
        destination.as_CFTypeRef().cast_mut()
      ));
    }
  }

  #[test]
  fn jpeg_applies_orientation_and_downsamples_without_source_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.jpg");
    fixture(&source);
    assert!(fs::read(&source)
      .unwrap()
      .windows(COMMENT.len())
      .any(|bytes| bytes == COMMENT.as_bytes()));
    let data = jpeg_bytes(&source, MAX_IMAGE_BYTES, || Ok(())).unwrap();
    assert!(data.bytes().starts_with(&[0xff, 0xd8, 0xff]));
    let output = directory.path().join("output.jpg");
    fs::write(&output, data.bytes()).unwrap();
    let converted = image_source(&output).unwrap();
    let properties = owned(
      unsafe {
        CGImageSourceCopyPropertiesAtIndex(converted.as_CFTypeRef().cast_mut(), 0, ptr::null())
          .cast()
      },
      "Could not inspect converted metadata.",
    )
    .unwrap();
    let properties: CFDictionary<CFString, CFType> =
      unsafe { CFDictionary::wrap_under_get_rule(properties.as_CFTypeRef().cast()) };
    let number = |key| {
      properties
        .find(cf_string(key))
        .unwrap()
        .downcast::<CFNumber>()
        .unwrap()
        .to_i64()
        .unwrap()
    };
    unsafe {
      assert_eq!(number(kCGImagePropertyPixelHeight), MAX_PIXEL_SIZE as i64);
      assert!(number(kCGImagePropertyPixelWidth) < MAX_PIXEL_SIZE as i64);
      if let Some(exif) = properties.find(cf_string(kCGImagePropertyExifDictionary)) {
        let exif: CFDictionary<CFString, CFType> =
          CFDictionary::wrap_under_get_rule(exif.as_CFTypeRef().cast());
        assert!(!exif.contains_key(&cf_string(kCGImagePropertyExifUserComment)));
      }
    }
    assert!(!data
      .bytes()
      .windows(COMMENT.len())
      .any(|bytes| bytes == COMMENT.as_bytes()));
  }

  #[test]
  fn conversion_checks_output_size_and_cancellation() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.jpg");
    fixture(&source);
    assert!(jpeg_bytes(&source, 1, || Ok(()))
      .unwrap_err()
      .contains("size limit"));
    let checks = Cell::new(0);
    let cancelled = jpeg_bytes(&source, MAX_IMAGE_BYTES, || {
      checks.set(checks.get() + 1);
      if checks.get() == 3 {
        Err("cancelled".into())
      } else {
        Ok(())
      }
    });
    assert_eq!(cancelled.unwrap_err(), "cancelled");
  }

  #[test]
  fn originals_are_preserved_and_oversize_conversion_leaves_no_staging_files() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.jpg");
    fixture(&source);
    let original = fs::read(&source).unwrap();
    let operation = Operation::begin("photo-copy-test", "copy").unwrap();
    let copied = directory.path().join("copy");
    stage_photo(&source, &copied, MAX_IMAGE_BYTES, false, &operation).unwrap();
    assert_eq!(fs::read(&copied).unwrap(), original);
    fs::remove_file(&copied).unwrap();
    let size_limit = original.len() as u64 - 1;
    assert!(stage_photo(&source, &copied, size_limit, true, &operation).is_err());
    assert!(!copied.exists());
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    fs::write(&copied, b"Existing staged bytes").unwrap();
    assert!(stage_photo(&source, &copied, MAX_IMAGE_BYTES, true, &operation).is_err());
    assert_eq!(fs::read(&copied).unwrap(), b"Existing staged bytes");
    assert_eq!(fs::read(&source).unwrap(), original);
  }

  #[link(name = "ImageIO", kind = "framework")]
  unsafe extern "C" {
    static kCGImagePropertyOrientation: CFStringRef;
    static kCGImagePropertyPixelWidth: CFStringRef;
    static kCGImagePropertyPixelHeight: CFStringRef;
    static kCGImagePropertyExifDictionary: CFStringRef;
    static kCGImagePropertyExifUserComment: CFStringRef;
    fn CGImageDestinationCreateWithURL(
      url: CFURLRef,
      kind: CFStringRef,
      count: usize,
      options: CFDictionaryRef,
    ) -> ImageDestinationRef;
    fn CGImageSourceCopyPropertiesAtIndex(
      source: ImageSourceRef,
      index: usize,
      options: CFDictionaryRef,
    ) -> CFDictionaryRef;
  }

  #[link(name = "CoreGraphics", kind = "framework")]
  unsafe extern "C" {
    fn CGColorSpaceCreateDeviceRGB() -> CGColorSpaceRef;
    fn CGColorSpaceRelease(colors: CGColorSpaceRef);
    fn CGBitmapContextCreate(
      data: *mut c_void,
      width: usize,
      height: usize,
      bits_per_component: usize,
      bytes_per_row: usize,
      colors: CGColorSpaceRef,
      bitmap_info: u32,
    ) -> CGContextRef;
    fn CGContextSetRGBFillColor(context: CGContextRef, red: f64, green: f64, blue: f64, alpha: f64);
    fn CGContextFillRect(context: CGContextRef, rectangle: CGRect);
    fn CGBitmapContextCreateImage(context: CGContextRef) -> CGImageRef;
    fn CGContextRelease(context: CGContextRef);
  }
}
