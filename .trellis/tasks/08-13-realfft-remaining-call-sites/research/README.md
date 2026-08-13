# Bench evidence

All numbers quoted in `docs/quality.md` for the 2026-08-13 `realfft` migration
come from interleaved paired runs, not sequential ones.

| Files | Case | Notes |
| --- | --- | --- |
| `sp-il-b{1,2}.json` / `sp-il-a{1,2}.json` | spectrum analyzer | before/after interleaved; downmixer cases in the same runs act as the unchanged control |
| `fir-il-b{1,2,3}.json` / `fir-il-a{1,2,3}.json` | FIR EQ regeneration | linear phase is the changed path; minimum phase in the same runs is the control, and drifted +5% while linear improved 7-17% |
| `il-b{1,2,3}.json` / `il-a{1,2,3}.json` | AutoMix analyze | interleaved: -4.1%, +1.0%, +1.0% -> reported as neutral |
| `comp-before.json` / `comp-after.json` | component suite | the original *sequential* pair, retained only because it is the run that falsely suggested a 4.7% AutoMix regression later attributed to host drift |

Superseded sequential FIR/component repeats were deleted; the interleaved sets
above supersede them.
