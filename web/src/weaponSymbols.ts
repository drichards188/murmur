// The icons of weapons on the map, one source for ours and for an opponent's (bd murmur-ma92):
// what the weapon is (missile, bomb, torpedo, gun round, or a weapon of a kind not known) in its
// silhouette, whose it is (friend, hostile, unknown) in the 2525C colours (milsymbol's own: the
// same blue, red and yellow the vehicles and tracks are drawn in, symbols.ts), and whether its
// position is observed or only predicted, in the 2525 convention for an anticipated position: a
// dashed outline and a see-through body. Pure functions (strings and numbers, no DOM), so
// weaponSymbols.check.ts can check them on their own; munitionLayers.ts draws them.
//
// Every icon is drawn nose up on a 40 x 88 canvas (turned about the middle of its body, which
// is what the layers' `getAngle` turns it by), at twice its on-screen size so it stays sharp on
// HiDPI as symbols.ts does, with a dark outline and a glow in its colour: it reads on the dark
// map, the water and the terrain alike.

export type WeaponAffiliation = 'friend' | 'hostile' | 'unknown'
export type WeaponSymbolKind = 'missile' | 'bomb' | 'torpedo' | 'gun' | 'unknown'

/** an IconLayer icon definition */
export interface WeaponIcon {
  id: string
  url: string
  width: number
  height: number
  anchorX: number
  anchorY: number
  mask: false
}

export type RGBA = [number, number, number, number]

/** each affiliation's colour: `main` is MIL-STD-2525C's frame colour as milsymbol draws it
 *  (friend rgb 128,224,255; hostile 255,128,128; unknown 255,255,128), `deep` a darker shade of it
 *  for details that must show against `main`; `rgba` and `pale` for the layers' lines and text */
export const WEAPON_COLORS: Record<WeaponAffiliation, { main: string; deep: string; rgba: RGBA; pale: RGBA }> = {
  friend: { main: '#80e0ff', deep: '#0b78a0', rgba: [128, 224, 255, 255], pale: [196, 242, 255, 255] },
  hostile: { main: '#ff8080', deep: '#a81c1c', rgba: [255, 128, 128, 255], pale: [255, 200, 200, 255] },
  unknown: { main: '#ffff80', deep: '#857a00', rgba: [255, 255, 128, 255], pale: [255, 255, 200, 255] },
}

/** the threat library's classes that fly as a missile does (the bridge's `physical::weapon`
 *  kinds, docs/OPPONENT-WEAPONS.md section 15): an ATGM, an air-launched missile and an unguided
 *  rocket share the missile's silhouette (at map scale a rocket's lack of control fins is not
 *  worth a shape of its own; its label names the class) */
const MISSILE_LIKE = ['atgm', 'air_missile', 'rocket']

/** the symbol kind for a catalog weapon kind ("missile", "bomb", "torpedo", "gun", "self",
 *  "mine"...) or a threat-library class's kind ("atgm", "air_missile", "rocket": a missile): the
 *  four that fly as themselves; anything else (and nothing reported) is a weapon of a kind not
 *  known */
export function weaponSymbolKind(kind: string | null | undefined): WeaponSymbolKind {
  const k = (kind ?? '').trim().toLowerCase()
  if (MISSILE_LIKE.includes(k)) return 'missile'
  return k === 'missile' || k === 'bomb' || k === 'torpedo' || k === 'gun' ? k : 'unknown'
}

export const ICON_WIDTH = 40
export const ICON_HEIGHT = 88
const ANCHOR_X = 20
const ANCHOR_Y = 38

interface Shape {
  /** the body's outline */
  body: string
  /** fins, tail cages and other parts drawn in the deep shade beside the body */
  parts: string
  /** drawn under everything: the missile's flame, the gun round's tracer streak */
  tail: string
  /** drawn over the body in the dark: marks that tell the kind apart at a glance */
  marks: string
}

const SHAPES: Record<WeaponSymbolKind, Shape> = {
  // slim body, pointed nose, four fins, a flame: the missile seen from the side
  missile: {
    body: 'M20 6 C24 10 26.5 15 26.5 20 L26.5 64 L13.5 64 L13.5 20 C13.5 15 16 10 20 6 Z',
    parts: 'M13.5 50 L6 62 L6 67 L13.5 64 Z M26.5 50 L34 62 L34 67 L26.5 64 Z M13.5 23 L9.5 31 L13.5 31 Z M26.5 23 L30.5 31 L26.5 31 Z',
    tail: 'M15.5 64 L20 84 L24.5 64 Z',
    marks: '',
  },
  // fat teardrop and a boxed tail cage, no flame: it falls
  bomb: {
    body: 'M20 8 C28 14 31 24 31 36 C31 47 27 55 23.5 59 L16.5 59 C13 55 9 47 9 36 C9 24 12 14 20 8 Z',
    parts: 'M12.5 59 L12.5 76 L27.5 76 L27.5 59 Z',
    tail: '',
    marks: 'M12.5 67.5 L27.5 67.5 M20 59 L20 76 M13 31 L27 31',
  },
  // long smooth cylinder, round nose, bands, a ducted propeller at the tail and no fins forward
  torpedo: {
    body: 'M13.5 20 C13.5 11 16.5 6 20 6 C23.5 6 26.5 11 26.5 20 L26.5 70 L13.5 70 Z',
    parts: 'M10.5 70 L29.5 70 L29.5 79 L10.5 79 Z',
    tail: '',
    marks: 'M13.5 30 L26.5 30 M13.5 58 L26.5 58 M20 70 L20 79 M10.5 74.5 L29.5 74.5',
  },
  // a short plump round with a tracer streak behind it
  gun: {
    body: 'M20 14 C25 19 26.5 27 26.5 34 L26.5 52 L13.5 52 L13.5 34 C13.5 27 15 19 20 14 Z',
    parts: 'M13.5 47 L26.5 47 L26.5 52 L13.5 52 Z',
    tail: 'M17 52 L20 82 L23 52 Z',
    marks: '',
  },
  // a plain capsule with no fins, flame or bands, and a question mark: a weapon, nothing more known
  unknown: {
    body: 'M20 8 C25.5 8 27.5 12 27.5 17 L27.5 59 C27.5 64 25.5 68 20 68 C14.5 68 12.5 64 12.5 59 L12.5 17 C12.5 12 14.5 8 20 8 Z',
    parts: '',
    tail: '',
    marks: '',
  },
}
// the "?" of the unknown kind, drawn as a path so no font is needed
const QUESTION = 'M16.8 31 C16.8 26.5 23.2 26.5 23.2 31 C23.2 34.5 20 34.5 20 38.5 M20 43 L20 45.5'

export interface WeaponIconOptions {
  affiliation: WeaponAffiliation
  kind: WeaponSymbolKind
  /** its position is a prediction, not an observation: the anticipated look (dashed) */
  predicted: boolean
}

export function weaponIconSvg(o: WeaponIconOptions): string {
  const c = WEAPON_COLORS[o.affiliation]
  const s = SHAPES[o.kind]
  const dark = '#0b0f14'
  const parts = s.parts ? `<path d="${s.parts}" fill="${c.deep}" fill-opacity="${o.predicted ? 0.6 : 1}" stroke="${dark}" stroke-width="1.8" stroke-linejoin="round"/>` : ''
  const tail = s.tail ? `<path d="${s.tail}" fill="${c.main}" fill-opacity="${o.predicted ? 0.5 : 0.95}"/>` : ''
  const marks = s.marks ? `<path d="${s.marks}" fill="none" stroke="${dark}" stroke-width="1.6" stroke-linecap="round"/>` : ''
  const question = o.kind === 'unknown' ? `<path d="${QUESTION}" fill="none" stroke="${dark}" stroke-width="3" stroke-linecap="round" stroke-linejoin="round"/>` : ''
  const outline = o.predicted
    ? // anticipated: a solid dark under-line with the colour dashed over it, the body see-through
      `<path d="${s.body}" fill="${c.main}" fill-opacity="0.32"/><path d="${s.body}" fill="none" stroke="${dark}" stroke-width="4.6" stroke-linejoin="round"/><path d="${s.body}" fill="none" stroke="${c.main}" stroke-width="2.4" stroke-linejoin="round" stroke-dasharray="4.5 3"/>`
    : `<path d="${s.body}" fill="${c.main}" stroke="${dark}" stroke-width="2.6" stroke-linejoin="round"/>`
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${ICON_WIDTH}" height="${ICON_HEIGHT}" viewBox="0 0 ${ICON_WIDTH} ${ICON_HEIGHT}">
<defs><filter id="g" x="-60%" y="-30%" width="220%" height="160%"><feGaussianBlur stdDeviation="3.2"/></filter></defs>
<path d="${s.body} ${s.parts}" fill="${c.main}" opacity="${o.predicted ? 0.55 : 0.8}" filter="url(#g)"/>
${tail}${outline}${parts}${marks}${question}
</svg>`
}

const cache = new Map<string, WeaponIcon>()

/** The IconLayer icon for a weapon: nose up, to be turned along its flight by `getAngle`; the same
 *  object for the same arguments, so deck.gl packs each into its atlas once. */
export function weaponIcon(o: WeaponIconOptions): WeaponIcon {
  const id = `weapon-${o.affiliation}-${o.kind}-${o.predicted ? 'predicted' : 'observed'}`
  let icon = cache.get(id)
  if (!icon) {
    icon = {
      id,
      url: `data:image/svg+xml;charset=utf-8,${encodeURIComponent(weaponIconSvg(o))}`,
      width: ICON_WIDTH,
      height: ICON_HEIGHT,
      anchorX: ANCHOR_X,
      anchorY: ANCHOR_Y,
      mask: false,
    }
    cache.set(id, icon)
  }
  return icon
}

export const WEAPON_KINDS: WeaponSymbolKind[] = ['missile', 'bomb', 'torpedo', 'gun', 'unknown']
export const WEAPON_AFFILIATIONS: WeaponAffiliation[] = ['friend', 'hostile', 'unknown']
