#import <Foundation/Foundation.h>
@import AuthenticationServices;
@import UIKit;
#include <stdint.h>

typedef void (*Completion)(void *context, int32_t status, const char *callbackURL);

enum {
  ApellAccountSucceeded = 0,
  ApellAccountCancelled = 1,
  ApellAccountFailed = 2,
};

@interface ApellAccountAuthentication : NSObject <ASWebAuthenticationPresentationContextProviding>
@property(nonatomic) uint64_t attemptID;
@property(nonatomic) void *context;
@property(nonatomic) Completion completion;
@property(nonatomic, strong) UIWindow *window;
@property(nonatomic, strong) ASWebAuthenticationSession *session;
@end

@implementation ApellAccountAuthentication
- (ASPresentationAnchor)presentationAnchorForWebAuthenticationSession:
    (ASWebAuthenticationSession *)session {
  (void)session;
  return self.window;
}
@end

// The session and its presentation context are owned by the main queue.
static ApellAccountAuthentication *activeAttempt;

static UIWindow *presentationWindow(void) {
  UIWindow *visibleWindow = nil;
  for (UIScene *scene in UIApplication.sharedApplication.connectedScenes) {
    if (scene.activationState != UISceneActivationStateForegroundActive ||
        ![scene isKindOfClass:UIWindowScene.class])
      continue;

    for (UIWindow *window in ((UIWindowScene *)scene).windows) {
      if (window.hidden || !window.rootViewController)
        continue;
      if (window.isKeyWindow)
        return window;
      if (!visibleWindow && window.windowLevel == UIWindowLevelNormal)
        visibleWindow = window;
    }
  }
  return visibleWindow;
}

static void finish(uint64_t attemptID, int32_t status, NSURL *callbackURL) {
  ApellAccountAuthentication *attempt = activeAttempt;
  if (!attempt || attempt.attemptID != attemptID)
    return;

  activeAttempt = nil;
  // The callback must copy the UTF-8 string before returning.
  (attempt.completion)(attempt.context, status, callbackURL.absoluteString.UTF8String);
}

void apell_account_present_ios(uint64_t attemptID, const char *authorizationURL, void *context,
                               Completion completion) {
  @autoreleasepool {
    NSString *address = authorizationURL ? @(authorizationURL) : nil;
    dispatch_async(dispatch_get_main_queue(), ^{
      UIWindow *window = presentationWindow();
      NSURL *url = address ? [NSURL URLWithString:address] : nil;
      if (!window || ![url.scheme isEqualToString:@"https"] || !url.host.length || activeAttempt) {
        completion(context, ApellAccountFailed, NULL);
        return;
      }

      NSString *callbackScheme = @"dev.apell.shell";
      ASWebAuthenticationSessionCompletionHandler handler = ^(NSURL *callbackURL, NSError *error) {
        dispatch_async(dispatch_get_main_queue(), ^{
          if (!error && [callbackURL.scheme isEqualToString:callbackScheme]) {
            finish(attemptID, ApellAccountSucceeded, callbackURL);
          } else if ([error.domain isEqualToString:ASWebAuthenticationSessionErrorDomain] &&
                     error.code == ASWebAuthenticationSessionErrorCodeCanceledLogin) {
            finish(attemptID, ApellAccountCancelled, nil);
          } else {
            finish(attemptID, ApellAccountFailed, nil);
          }
        });
      };

      ApellAccountAuthentication *attempt = [ApellAccountAuthentication new];
      attempt.attemptID = attemptID;
      attempt.context = context;
      attempt.completion = completion;
      attempt.window = window;
      if (@available(iOS 17.4, *)) {
        ASWebAuthenticationSessionCallback *callback =
            [ASWebAuthenticationSessionCallback callbackWithCustomScheme:callbackScheme];
        attempt.session = [[ASWebAuthenticationSession alloc] initWithURL:url
                                                                 callback:callback
                                                        completionHandler:handler];
      } else {
        attempt.session = [[ASWebAuthenticationSession alloc] initWithURL:url
                                                        callbackURLScheme:callbackScheme
                                                        completionHandler:handler];
      }
      attempt.session.presentationContextProvider = attempt;
      activeAttempt = attempt;
      if (![attempt.session start])
        finish(attemptID, ApellAccountFailed, nil);
    });
  }
}

void apell_account_cancel_ios(uint64_t attemptID) {
  dispatch_async(dispatch_get_main_queue(), ^{
    if (activeAttempt && activeAttempt.attemptID == attemptID) {
      [activeAttempt.session cancel];
      finish(attemptID, ApellAccountCancelled, nil);
    }
  });
}
