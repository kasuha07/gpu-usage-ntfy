# Changelog

## [0.2.1](https://github.com/kasuha07/gpu-usage-ntfy/compare/gpu-usage-ntfy-v0.2.0...gpu-usage-ntfy-v0.2.1) (2026-03-09)


### Bug Fixes

* support component-style release tags ([c0f563e](https://github.com/kasuha07/gpu-usage-ntfy/commit/c0f563e7666307e646b5a9cd199e51588a955796))

## [0.2.0](https://github.com/kasuha07/gpu-usage-ntfy/compare/v0.1.0...gpu-usage-ntfy-v0.2.0) (2026-03-09)


### Features

* add interactive systemd installer ([95ec19a](https://github.com/kasuha07/gpu-usage-ntfy/commit/95ec19a4f2435226c79e0a7acf8c491c96ca36cf))
* add runtime helpers and ntfy auth validation ([817454d](https://github.com/kasuha07/gpu-usage-ntfy/commit/817454d790d2f7871b64566cd4cd1ce44dcc5fdf))
* add rust gpu usage monitor with ntfy notifications ([e89f8de](https://github.com/kasuha07/gpu-usage-ntfy/commit/e89f8de1a8df98edf20edd01092b8531b10e8036))
* aggregate idle notifications before dedup ([9ae9a75](https://github.com/kasuha07/gpu-usage-ntfy/commit/9ae9a75702a2f25be7d5a5b9783cab6d35160364))
* batch ntfy alerts and make idle repeats optional ([b57c5b3](https://github.com/kasuha07/gpu-usage-ntfy/commit/b57c5b304c5c228d9f42c9c5ddf8914818df417d))
* hot-reload config and localize notifications ([9ead687](https://github.com/kasuha07/gpu-usage-ntfy/commit/9ead687b6125d79ecf152f038b516131d88e6578))
* switch notifications to idle-state policy ([4da5970](https://github.com/kasuha07/gpu-usage-ntfy/commit/4da5970c3280487f4e6f9587d2be4edd82df4a55))


### Bug Fixes

* harden ntfy request encoding ([26e020c](https://github.com/kasuha07/gpu-usage-ntfy/commit/26e020c1a11d73de3df46550df173078073e5bf3))
* load NVML from trusted absolute library paths ([18a8086](https://github.com/kasuha07/gpu-usage-ntfy/commit/18a80869dd06906b0c1e9d57954172e262db6c16))
* preserve alert state across safe reloads ([6b2c498](https://github.com/kasuha07/gpu-usage-ntfy/commit/6b2c4982f3276eb81c0df3d51b675b4011a8e68d))
* quote ops script paths and harden systemd unit ([6dd4368](https://github.com/kasuha07/gpu-usage-ntfy/commit/6dd43684b223218ceb3af4e4f0ec98aef7854787))
* restart stale ntfy monitor deployments ([06e1c0d](https://github.com/kasuha07/gpu-usage-ntfy/commit/06e1c0dac68632c5dec6768ddfaff2f7a7a1d236))
* validate ntfy timeout and server URLs ([911c578](https://github.com/kasuha07/gpu-usage-ntfy/commit/911c5784dfd1dc6540993b3803e4884ca3d55b42))
