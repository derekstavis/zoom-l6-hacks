# Emulator test readiness investigation

The figures below are diagnostic observations, not a benchmark or proof of
physical-device behavior. Guest-time waits do not necessarily equal host wall
time. Machine load can change the wall-time measurements. All compared runs
used externally supplied QEMU SHA-256
`d33f3853e30115234b1deb9c4b727dfed13a234fcdece2fa18ffe05e7d692a62`;
the driver pins that binary but does not attest its source. Stock firmware
packages were L6 `64f1f36b8383176b5d841911fbf3c16205139a313ee88be906f313b1a98a33fb`
and L6max `4e20f92c9c5131ed082c0feec2b2d7d0445fb576296f11f6dace56a4fc0c9b47`.
See the run reports for scenario, runner, state and card identities.

## Editor startup

The `editor-pad-assignments` case includes both model-specific setup fragments.
Each previously began with an unscoped 40-second guest-time delay. Consequently
both models waited twice, even when a state spec had already completed setup.
The USB fragment added a further five seconds. In a prior L6max run the
completed-step records show 40,012 ms, 80,087 ms and 85,137 ms before USB
enumeration. The earlier full suite passed Editor assignment in 101.1 seconds
on L6 and 105.3 seconds on L6max, including fixture creation.

`usb-descriptors` already attaches the cable and polls the emulated USB
controller until it runs, bounded by guest and wall deadlines. An experiment
that removed the waits and relied only on this signal passed on L6 but failed
on L6max: enumeration and the Editor identity exchange succeeded, yet the
global-settings request received no response. USB controller readiness alone
is not sufficient to prove that the Editor service is ready.

A second experiment waited for the existing `boot-ready` panel signal before
USB enumeration. With setup-complete state, both Editor assignment models
passed: L6 in 20.7 seconds and L6max in 25.8 seconds, including fixtures. A
repeat passed in 20.8 and 25.8 seconds. L6 Editor-assigned pad playback passed
in 26.3 seconds, versus 67.3 seconds in the earlier suite. The L6max
on-device assignment case passed in 187.9 seconds, versus 227.1 seconds in
the earlier suite; its current checks do not prove rendered-pixel fidelity. These are individual
runs, not a statistically controlled performance comparison. The tests still
check the complete Editor exchange and assignment readback; the readiness
check is an additional prerequisite, not a substitute for those assertions.
The real-device startup timing and behavior are not established by QEMU runs.

The 40-second delay is retained for cases that actually perform first-boot
setup; replacing it requires a separate, model-specific prompt-ready signal and
validation from fresh state. Model-scoped setup fragments should not impose
one model's waiting on the other. Other users of the shared USB fragment need
independent checks, including rendered-pixel evidence when asserting GUI
behavior.

## Other candidates

The L6max on-device assignment helper also waits 1.5 guest seconds after
each acknowledged key, adds two seconds before menu navigation keys and five
seconds for a possible popup. These are further candidates for menu or
reviewed display-image readiness checks; the current work leaves them intact.

A static scan found 84 checked-in `delay` steps of at least five guest seconds
across case and fragment TOML files. This is a count of declarations, not of
executed steps or measured suite duration: includes, model scopes and skipped
setup steps change what runs. Triage candidates against actual case timings:

- MIDI and SD-patch cases have 40-second startup waits; evaluate boot readiness,
  USB controller readiness and the relevant protocol or patch-loaded response
  separately. The Editor result shows why USB readiness alone is insufficient.
- Fresh-boot and power cases wait for startup prompts. Look for model-specific
  prompt state or rendered display evidence before replacing their waits.
- Audio and recorder cases may use waits to collect a required waveform or
  observation window. Do not remove those solely because they are long;
  investigate an output or state predicate that still proves the assertion.

Preserve the original timing for the cases being investigated, compare with
identical firmware and executable identities, exercise both models and relevant
backends, and report failures or retries as such. A faster pass under one set
of host conditions is insufficient evidence of reliability.
