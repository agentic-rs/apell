mod back_gesture;
mod wake_lease;

pub(crate) use back_gesture::BackSignal;
pub(crate) use wake_lease::WakeLease;

use objc2::{msg_send, rc::Retained, runtime::AnyObject};
use objc2_ui_kit::{UIScrollView, UIScrollViewContentInsetAdjustmentBehavior, UIView};
use tauri::Manager;

pub fn configure_webview(window: &tauri::WebviewWindow, back: BackSignal) -> tauri::Result<()> {
  back.bind(window.app_handle().clone());
  window.with_webview(move |platform| {
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

    // SAFETY: WKWebView is a UIView, valid for this main-thread callback.
    let view = unsafe { &*platform.inner().cast::<UIView>() };
    back_gesture::attach(view, &back);
  })
}
