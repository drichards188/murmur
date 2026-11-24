// Checks for weaponSymbols.ts and the label and tracer helpers of munition.ts (bd murmur-ma92):
//   node web/src/weaponSymbols.check.ts
// Throws on the first failure, prints "weaponSymbols: ok" otherwise. Not part of the bundle.
import { burstShown, BURST_S, flightGroupKey, flightGroupLabel, flightLabel, tracerHeads, type MunitionFlight } from './munition.ts'
import { weaponIcon, weaponIconSvg, weaponSymbolKind, WEAPON_AFFILIATIONS, WEAPON_KINDS } from './weaponSymbols.ts'

function check(cond: boolean, what: string) {
  if (!cond) throw new Error(`weaponSymbols check failed: ${what}`)
}

// the catalog's kinds map; what flies as nothing known (self, mine, nothing reported) is "unknown"
check(weaponSymbolKind('missile') === 'missile', 'missile')
check(weaponSymbolKind('Torpedo') === 'torpedo', 'case')
check(weaponSymbolKind('bomb') === 'bomb' && weaponSymbolKind('gun') === 'gun', 'bomb, gun')
check(weaponSymbolKind('self') === 'unknown' && weaponSymbolKind('mine') === 'unknown', 'self, mine')
// the threat library's classes (the weapon picture's `class_symbol` is already one of the four)
check(weaponSymbolKind('atgm') === 'missile' && weaponSymbolKind('air_missile') === 'missile' && weaponSymbolKind('Rocket') === 'missile', 'atgm, air missile, rocket fly as missiles')
check(weaponSymbolKind(undefined) === 'unknown' && weaponSymbolKind(null) === 'unknown' && weaponSymbolKind('') === 'unknown', 'none')
// a catalog missile flying the dive profile is a drone (bd murmur-4nik.5); the profile changes
// nothing else
check(weaponSymbolKind('missile', 'dive') === 'drone' && weaponSymbolKind('Missile', 'Dive') === 'drone', 'a dive missile is a drone')
check(weaponSymbolKind('missile', 'lofted') === 'missile' && weaponSymbolKind('missile', null) === 'missile', 'other missiles stay missiles')
check(weaponSymbolKind('self', 'dive') === 'unknown' && weaponSymbolKind('bomb', 'dive') === 'bomb' && weaponSymbolKind('atgm', 'dive') === 'missile', 'only a catalog missile')

// every variant: its own id, a drawing of its own, the contract's shape; the same object again
const seen = new Set<string>()
const drawings = new Set<string>()
for (const affiliation of WEAPON_AFFILIATIONS)
  for (const kind of WEAPON_KINDS)
    for (const predicted of [false, true]) {
      const i = weaponIcon({ affiliation, kind, predicted })
      seen.add(i.id)
      drawings.add(weaponIconSvg({ affiliation, kind, predicted }))
      check(i.mask === false && i.width === 40 && i.height === 88 && i.anchorX === 20, `${i.id} contract`)
      check(i.url.startsWith('data:image/svg+xml'), `${i.id} url`)
      check(weaponIcon({ affiliation, kind, predicted }) === i, `${i.id} cached`)
      check(predicted === weaponIconSvg({ affiliation, kind, predicted }).includes('stroke-dasharray'), `${i.id} dashed only when predicted`)
    }
check(seen.size === 36 && drawings.size === 36, 'thirty-six distinct icons')
// the drone from above: four rotor discs round a body, its X frame under them, turned about the
// body's middle (the anchor, as every icon)
const quad = weaponIconSvg({ affiliation: 'friend', kind: 'drone', predicted: true })
const arcs = (quad.match(/A6 6 0 1 0/g) ?? []).length
check(arcs > 0 && arcs % 8 === 0, 'four rotor discs, two arcs each, in every path of the body')
// after the glow (the first of each), the frame's own path comes before the body's
const second = (sub: string) => quad.indexOf(sub, quad.indexOf(sub) + 1)
check(second('M9.7 25.3') > 0 && second('M9.7 25.3') < second('M2.5 26.5 A6'), 'the frame drawn under the discs')
check(weaponIcon({ affiliation: 'hostile', kind: 'drone', predicted: false }).anchorY === weaponIcon({ affiliation: 'friend', kind: 'missile', predicted: false }).anchorY, 'the same anchor')

// the label: weapon, launcher > target, seconds, PRED
const f: MunitionFlight = {
  m: { name: 'Hellfire-class missile', speed_mps: 425, launch_t: 0, launch_point: [0, 0], launch_alt_m: 0, impact_t: 9_000, impact_point: [0, 0], impact_alt_m: 0, tof_s: 9 },
  kind: 'missile',
  launcher: 'REAPER 1',
  target: 'T-12',
}
check(flightLabel(f, 0) === 'HELLFIRE · REAPER 1 > T-12 · 9 s PRED', flightLabel(f, 0))
check(flightLabel({ ...f, launcher: undefined, target: undefined }, 8_500) === 'HELLFIRE · 1 s PRED', 'bare label')
check(flightLabel({ ...f, target: undefined }, 0) === 'HELLFIRE · REAPER 1 · 9 s PRED', 'no target')

// a salvo's rounds (rules 35): one weapon, launcher and target are one group, one label with the
// rounds in flight and the seconds to the first and the last impact; one round is its own label
const round = (k: number): MunitionFlight => ({
  m: { name: 'HIV3-class launcher round', speed_mps: 20, launch_t: k * 2000, launch_point: [0, 0], launch_alt_m: 0, impact_t: 20_000 + k * 2000, impact_point: [0, 0], impact_alt_m: 0, tof_s: 20 },
  kind: 'missile',
  profile: 'dive',
  launcher: 'HIVE 1',
  target: 'T-0001',
})
const salvo = [0, 1, 2, 3, 4, 5].map(round)
check(new Set(salvo.map(flightGroupKey)).size === 1, 'one salvo, one group')
check(flightGroupKey({ ...round(0), target: 'T-0002' }) !== flightGroupKey(round(0)), 'another target, another group')
check(flightGroupKey({ ...round(0), launcher: 'HIVE 2' }) !== flightGroupKey(round(0)), 'another launcher, another group')
check(flightGroupKey(f) !== flightGroupKey({ ...f, m: { ...f.m, name: 'GBU-12 Paveway II' } }), 'another weapon, another group')
check(flightGroupLabel(salvo, 10_000) === 'HIV3 ×6 · HIVE 1 > T-0001 · 10–20 s PRED', flightGroupLabel(salvo, 10_000))
check(flightGroupLabel([round(0), round(0)], 0) === 'HIV3 ×2 · HIVE 1 > T-0001 · 20 s PRED', 'one window, one figure')
check(flightGroupLabel([round(3)], 10_000) === flightLabel(round(3), 10_000), 'a group of one is the round\'s own label')

// tracers: only for a reported burst, only while it lasts
const fire = { bursts: 3, rounds: 18, last_t: 100_000 }
check(tracerHeads({ bursts: 0, rounds: 0, last_t: 0 }, 0).length === 0, 'no burst reported, nothing')
check(tracerHeads(fire, 99_999).length === 0, 'before the burst')
check(tracerHeads(fire, 100_050).length === 1, 'first round out')
check(tracerHeads(fire, 100_000 + BURST_S * 1000 + 400).length === 0, 'after it')
check(tracerHeads(fire, 100_400).every((p) => p >= 0 && p < 1), 'fractions on the line')
check(tracerHeads(fire, 100_300, true).length === 1 && tracerHeads(fire, 100_900, true).length === 0, 'reduced motion: one still line for the burst')
check(burstShown(fire, 99_500) && !burstShown(fire, 102_000) && !burstShown({ ...fire, bursts: 0 }, 100_100), 'animation frames')

console.log('weaponSymbols: ok')
