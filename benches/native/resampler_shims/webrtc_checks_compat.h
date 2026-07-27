#ifndef AEB_WEBRTC_CHECKS_COMPAT_H
#define AEB_WEBRTC_CHECKS_COMPAT_H

// The standalone resampler sources only need the check macros. The packaged
// WebRTC checks header also pulls in Abseil, which is unrelated to this shim.
#define RTC_BASE_CHECKS_H_
#define RTC_DCHECK_IS_ON 0

#include <cstdlib>

namespace aeb_webrtc_compat {

[[noreturn]] inline void check_failed() { std::abort(); }

}  // namespace aeb_webrtc_compat

#define RTC_CHECK(condition)                                                \
  ((condition) ? static_cast<void>(0)                                      \
               : ::aeb_webrtc_compat::check_failed())
#define RTC_CHECK_EQ(left, right) RTC_CHECK((left) == (right))
#define RTC_CHECK_GE(left, right) RTC_CHECK((left) >= (right))

#define RTC_DCHECK(condition) static_cast<void>(0)
#define RTC_DCHECK_EQ(left, right) static_cast<void>(0)
#define RTC_DCHECK_GE(left, right) static_cast<void>(0)
#define RTC_DCHECK_GT(left, right) static_cast<void>(0)
#define RTC_DCHECK_LT(left, right) static_cast<void>(0)

#endif
