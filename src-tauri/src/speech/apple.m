// File-based system speech recognition, called on a blocking worker.
#import <Foundation/Foundation.h>
@import Speech;
#include <stdbool.h>

typedef bool (*Cancellation)(const void *);

static int waitForSignal(dispatch_semaphore_t signal, Cancellation cancelled, const void *context) {
  while (!cancelled(context)) {
    if (dispatch_semaphore_wait(signal, dispatch_time(DISPATCH_TIME_NOW, 100 * NSEC_PER_MSEC)) == 0)
      return 1;
  }
  return 0;
}

// A null reason means that the recognizer can be selected. Language-specific
// availability is checked when opening a file, using that job's language.
const char *apell_system_speech_unavailable_reason(void) {
  @autoreleasepool {
    if (![[NSBundle.mainBundle objectForInfoDictionaryKey:@"NSSpeechRecognitionUsageDescription"]
            length])
      return "Use the installed Apell app for system speech.";
    switch (SFSpeechRecognizer.authorizationStatus) {
    case SFSpeechRecognizerAuthorizationStatusDenied:
      return "Allow speech recognition in device privacy settings.";
    case SFSpeechRecognizerAuthorizationStatusRestricted:
      return "Speech recognition is restricted on this device.";
    default:
      return NULL;
    }
  }
}

// The caller owns the returned UTF-8 buffer. All callbacks retain only native
// state so OS callbacks arriving after cancellation never touch the Rust job.
char *apell_system_transcribe(const char *path, const char *language, Cancellation cancelled,
                              const void *context) {
  @autoreleasepool {
    const char *reason = apell_system_speech_unavailable_reason();
    if (reason)
      return strdup(reason);
    if (cancelled(context))
      return strdup("Speech operation cancelled.");
    SFSpeechRecognizerAuthorizationStatus status = SFSpeechRecognizer.authorizationStatus;
    if (status == SFSpeechRecognizerAuthorizationStatusNotDetermined) {
      dispatch_semaphore_t permission = dispatch_semaphore_create(0);
      [SFSpeechRecognizer requestAuthorization:^(SFSpeechRecognizerAuthorizationStatus result) {
        (void)result;
        dispatch_semaphore_signal(permission);
      }];
      if (!waitForSignal(permission, cancelled, context))
        return strdup("Speech operation cancelled.");
      status = SFSpeechRecognizer.authorizationStatus;
    }
    if (status != SFSpeechRecognizerAuthorizationStatusAuthorized)
      return strdup("Allow speech recognition for Apell in device privacy settings.");
    NSLocale *locale =
        language ? [NSLocale localeWithLocaleIdentifier:@(language)] : NSLocale.currentLocale;
    SFSpeechRecognizer *recognizer = [[SFSpeechRecognizer alloc] initWithLocale:locale];
    if (!recognizer.supportsOnDeviceRecognition || !recognizer.available)
      return strdup("System recognition is unavailable for this language. Check device speech "
                    "settings or choose another recognizer.");
    NSURL *file = [NSURL fileURLWithPath:@(path)];
    SFSpeechURLRecognitionRequest *request =
        [[SFSpeechURLRecognitionRequest alloc] initWithURL:file];
    request.requiresOnDeviceRecognition = YES;
    request.shouldReportPartialResults = NO;
    dispatch_semaphore_t finished = dispatch_semaphore_create(0);
    __block NSData *transcript = nil;
    SFSpeechRecognitionTask *task = [recognizer
        recognitionTaskWithRequest:request
                     resultHandler:^(SFSpeechRecognitionResult *result, NSError *error) {
                       if (!error && !result.isFinal)
                         return;
                       if (result.isFinal) {
                         SFTranscription *best = result.bestTranscription;
                         NSMutableArray *segments = [NSMutableArray new];
                         for (SFTranscriptionSegment *part in best.segments) {
                           [segments addObject:@{
                             @"text" : part.substring,
                             @"start_ms" : @(llround(part.timestamp * 1000)),
                             @"end_ms" : @(llround((part.timestamp + part.duration) * 1000))
                           }];
                         }
                         transcript = [NSJSONSerialization dataWithJSONObject:@{
                           @"text" : best.formattedString,
                           @"language" : [recognizer.locale.localeIdentifier
                               stringByReplacingOccurrencesOfString:@"_"
                                                         withString:@"-"],
                           @"model_id" : @"system",
                           @"segments" : segments
                         }
                                                                      options:0
                                                                        error:nil];
                       }
                       dispatch_semaphore_signal(finished);
                     }];
    if (!waitForSignal(finished, cancelled, context)) {
      [task cancel];
      return strdup("Speech operation cancelled.");
    }
    if (!transcript.length || transcript.length > 4 * 1024 * 1024)
      return strdup("System recognition could not finish this recording. Try a shorter clip or "
                    "another recognizer.");
    return strndup(transcript.bytes, transcript.length);
  }
}

void apell_free_speech_reply(char *response) { free(response); }
