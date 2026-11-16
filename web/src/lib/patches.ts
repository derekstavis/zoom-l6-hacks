import catalog from '../generated/patch-catalog.json'

type Package = (typeof catalog.packages)[number]

export interface PatchInfo {
  id: string
  models: readonly ('L6' | 'L6max')[]
  title: string
  author: string
  description: string
  readmeUrl: string
  demoImage?: string
  requestCommand?: string
}

export interface LoadablePatchInfo {
  id: string
  models: readonly ('L6' | 'L6max')[]
  title: string
  description: string
  /** Versioned filename hosted under `public/loadable-patches/<model>/`. */
  fileName: string
  order: number
}

export interface EffectReplacementInfo {
  id: string
  title: string
  author: string
  description: string
  models: readonly ('L6' | 'L6max')[]
  packageId: string
  packageVersion: string
  readmeUrl: string
}

const readmeUrl = (patch: Package) =>
  `https://github.com/derekstavis/zoom-l6max/blob/main/patches/${patch.name}/${patch.readme}`
const models = (values: string[]) => values as ('L6' | 'L6max')[]

const visiblePackages = catalog.packages
  .filter((patch) => patch.website)
  .toSorted((left, right) => left.website_order - right.website_order)

export const PATCHES: PatchInfo[] = visiblePackages.map((patch) => ({
  id: patch.name,
  models: models(patch.models),
  title: patch.title,
  author: patch.author,
  description: patch.description,
  readmeUrl: readmeUrl(patch),
  ...(patch.demo_image ? { demoImage: patch.demo_image } : {}),
  ...(patch.request_command ? { requestCommand: patch.request_command } : {}),
}))

/** Packages with published SD binaries for the website's installation flow. */
export const LOADABLE_PATCHES: LoadablePatchInfo[] = visiblePackages
  .filter((patch) => patch.website_sd_models.length > 0)
  .map((patch) => ({
    id: patch.name,
    models: models(patch.website_sd_models),
    title: patch.title,
    description: patch.description,
    fileName: patch.file_name,
    order: patch.order,
  }))

export const EFFECT_REPLACEMENTS: EffectReplacementInfo[] = catalog.packages
  .flatMap((patch) =>
    (patch.effects ?? []).map((effect) => ({
      id: effect.id,
      title: effect.title,
      author: patch.author,
      description: patch.description,
      models: models(patch.models),
      packageId: patch.name,
      packageVersion: patch.version,
      readmeUrl: readmeUrl(patch),
    })),
  )

export function effectReplacementFileName(replacements: Record<string, string>) {
  const selectedIds = [...new Set(Object.values(replacements).filter(Boolean))]
  const effectPackages = selectedIds
    .map((id) => EFFECT_REPLACEMENTS.find((effect) => effect.id === id))
    .filter((effect): effect is EffectReplacementInfo => effect !== undefined)
    .map((effect) => effect.packageId)
  const packages = new Map(catalog.packages.map((pkg) => [pkg.name, pkg]))
  const closure = new Set<string>()
  const include = (name: string) => {
    if (closure.has(name)) return
    closure.add(name)
    packages.get(name)?.requires.forEach(include)
  }
  include('efx-loader')
  effectPackages.forEach(include)
  const remaining = new Set(closure)
  const ordered: string[] = []
  while (remaining.size > 0) {
    const next = [...remaining]
      .filter((name) => packages.get(name)?.requires.every((dependency) => !remaining.has(dependency)))
      .sort((a, b) => (packages.get(a)?.order ?? 0) - (packages.get(b)?.order ?? 0) || a.localeCompare(b))[0]
    if (!next) break
    remaining.delete(next)
    ordered.push(next)
  }
  const packagePart = ordered
    .filter((name) => name !== 'efx-loader')
    .map((name) => `${name}-${packages.get(name)?.version ?? '0.0.0'}`)
    .join('-')
  const room = replacements.room || 'stock'
  const spring = replacements.spring || 'stock'
  return `efx-loader-${packagePart}-room-${room}-spring-${spring}.bin`
}
