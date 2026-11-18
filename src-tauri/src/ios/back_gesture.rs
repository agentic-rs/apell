use objc2::{
  define_class, ffi, msg_send,
  rc::Retained,
  runtime::{AnyObject, ProtocolObject},
  sel, ClassType, DefinedClass, MainThreadOnly,
};
use objc2_foundation::{NSObject, NSObjectProtocol};
use objc2_ui_kit::{
  UIGestureRecognizer, UIGestureRecognizerDelegate, UIGestureRecognizerState, UIRectEdge,
  UIScreenEdgePanGestureRecognizer, UIView,
};
use std::sync::{Arc, OnceLock, Weak};
use tauri::Emitter;

const MIN_DISTANCE: f64 = 64.;
const HORIZONTAL_BIAS: f64 = 1.2;

type AppSlot = OnceLock<tauri::AppHandle>;

/// Owned by `run` so the native view cannot keep the Tauri runtime alive.
#[derive(Clone, Default)]
pub(crate) struct BackSignal(Arc<AppSlot>);

impl BackSignal {
  pub(crate) fn bind(&self, app: tauri::AppHandle) {
    let _ = self.0.set(app);
  }
}

struct BackTargetIvars(Weak<AppSlot>);

define_class!(
  // SAFETY: NSObject has no subclass-specific initialization requirements.
  #[unsafe(super = NSObject)]
  #[thread_kind = MainThreadOnly]
  #[ivars = BackTargetIvars]
  struct BackTarget;

  unsafe impl NSObjectProtocol for BackTarget {}

  unsafe impl UIGestureRecognizerDelegate for BackTarget {
    #[unsafe(method(gestureRecognizer:shouldRecognizeSimultaneouslyWithGestureRecognizer:))]
    fn allow_simultaneous(&self, _: &UIGestureRecognizer, _: &UIGestureRecognizer) -> bool {
      // The edge observer must not steal WebKit's own scroll/selection gestures.
      true
    }
  }

  impl BackTarget {
    #[unsafe(method(apellEdgePan:))]
    fn edge_pan(&self, pan: &UIScreenEdgePanGestureRecognizer) {
      if pan.state() != UIGestureRecognizerState::Ended {
        return;
      }
      let translation = pan.translationInView(None);
      if !(translation.x >= MIN_DISTANCE && translation.x > HORIZONTAL_BIAS * translation.y.abs()) {
        return;
      }
      let Some(signal) = self.ivars().0.upgrade() else { return };
      let Some(app) = signal.get() else { return };
      let _ = app.emit_to("main", "apell-back", ());
    }
  }
);

pub(super) fn attach(view: &UIView, signal: &BackSignal) {
  // UIKit's target and delegate do not own the object. An associated value
  // makes the recognizer responsible for its lifetime alongside the view.
  static TARGET_KEY: u8 = 0;
  let main = view.mtm();
  let target = BackTarget::alloc(main).set_ivars(BackTargetIvars(Arc::downgrade(&signal.0)));
  // SAFETY: NSObject's init needs no arguments; the action's parameter is the
  // recognizer being installed; the view and recognizer are on the main thread.
  let (target, pan) = unsafe {
    let target: Retained<BackTarget> = msg_send![super(target), init];
    let pan = UIScreenEdgePanGestureRecognizer::initWithTarget_action(
      UIScreenEdgePanGestureRecognizer::alloc(main),
      Some(target.as_super().as_ref()),
      Some(sel!(apellEdgePan:)),
    );
    ffi::objc_setAssociatedObject(
      Retained::as_ptr(&pan).cast::<AnyObject>().cast_mut(),
      (&TARGET_KEY as *const u8).cast(),
      Retained::as_ptr(&target).cast::<AnyObject>().cast_mut(),
      ffi::OBJC_ASSOCIATION_RETAIN_NONATOMIC,
    );
    (target, pan)
  };
  pan.setEdges(UIRectEdge::Left);
  pan.setMaximumNumberOfTouches(1);
  pan.setCancelsTouchesInView(false);
  pan.setDelegate(Some(ProtocolObject::from_ref(&*target)));
  view.addGestureRecognizer(&pan);
}
