use objc2::{msg_send, rc::Retained, runtime::AnyObject};
use objc2_ui_kit::{UIScrollView, UIScrollViewContentInsetAdjustmentBehavior};

pub fn configure_safe_area(window: &tauri::WebviewWindow) -> tauri::Result<()> {
  window.with_webview(|platform| {
    // SAFETY: Tauri supplies a live WKWebView and runs this callback on the main
    // thread. WKWebView's public scrollView getter returns a UIScrollView.
    let scroll_view: Retained<UIScrollView> = unsafe {
      let webview = &*platform.inner().cast::<AnyObject>();
      msg_send![webview, scrollView]
    };
    // viewport-fit=cover and CSS safe-area padding already handle the device
    // edges. UIKit must not add the same insets to the web content again.
    scroll_view
      .setContentInsetAdjustmentBehavior(UIScrollViewContentInsetAdjustmentBehavior::Never);
  })
}
