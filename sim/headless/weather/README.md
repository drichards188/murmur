# Pinned weather for Fog watch

`fog-watch.json` contains the hourly rows bracketing 2026-06-14 19:00 to 19:10 UTC, at the
Golden Gate's 20 requested locations. It was extracted from the cached HRRR archive on
2026-10-29; `fetched_at`, the API query, raw response checksum and parent artifact checksum
are in the file. The raw data remains in the local Tidewater cache.

Weather data by [Open-Meteo](https://open-meteo.com/), NOAA HRRR. Open-Meteo data is provided
under [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/); retain this attribution when
redistributing this data. The source's API service terms are separate from the data license.
This is archived model output, not observed local weather or a forecast issued by Murmur.

The synthetic visibility timeline is in `sim/scenarios/fog-watch.toml`, not in this artifact.
See [weather documentation](../../../docs/WEATHER.md) for sampling, limits and regeneration.
