// Display labels for catalog fields (sim/platforms.toml), shared by the platform and vehicle views.

/** How a C2 reaches the real platform (sim/platforms.toml, docs/AUTHENTICITY.md). */
export type Protocol = 'mavlink' | 'stanag4586' | 'uci' | 'jaus' | 'umaa' | 'proprietary' | 'rc' | 'telemetry'

export const LINKS: Record<string, string> = {
  los: 'Line-of-sight radio',
  mesh: 'Mesh radio',
  satcom: 'SATCOM',
  acoustic: 'Acoustic modem; SATCOM when surfaced',
  tether: 'Tether',
}

export const PROTOCOLS: Record<Protocol, string> = {
  mavlink: 'MAVLink',
  stanag4586: 'STANAG 4586',
  uci: 'UCI',
  jaus: 'JAUS',
  umaa: 'UMAA/DDS',
  proprietary: 'Proprietary',
  rc: 'RC link',
  telemetry: 'Telemetry only',
}
