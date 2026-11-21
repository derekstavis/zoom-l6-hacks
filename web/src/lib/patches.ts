import catalog from '../generated/patch-catalog.json'

type Package = (typeof catalog.packages)[number]

export interface PatchInfo {
  id: string
  models: readonly ('L6' | 'L6max')[]
  title: string
  author: string
  version: string
  dependencies: { name: string; version: string }[]
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
  version: string
  dependencies: { name: string; version: string }[]
  description: string
  models: readonly ('L6' | 'L6max')[]
  packageId: string
  packageVersion: string
  readmeUrl: string
  basedOn: { title: string; url: string } | null
  learnMore: { title: string; url: string } | null
}

const readmeUrl = (patch: Package) =>
  `https://github.com/derekstavis/zoom-l6max/blob/main/patches/${patch.name}/${patch.readme}`
const models = (values: string[]) => values as ('L6' | 'L6max')[]

/** Dependencies included in the same firmware image or SD module as their consumers. */
export function patchDependencies(roots: Iterable<string>): Set<string> {
  const packages = new Map(catalog.packages.map((pkg) => [pkg.name, pkg]))
  const dependencies = new Set<string>()
  const visit = (name: string) => {
    for (const dependency of packages.get(name)?.requires ?? []) {
      if (!dependencies.has(dependency.name)) {
        dependencies.add(dependency.name)
        visit(dependency.name)
      }
    }
  }
  for (const root of roots) visit(root)
  return dependencies
}

const visiblePackages = catalog.packages
  .filter((patch) => patch.website)
  .toSorted((left, right) => left.website_order - right.website_order)

export const PATCHES: PatchInfo[] = visiblePackages.map((patch) => ({
  id: patch.name,
  models: models(patch.models),
  title: patch.title,
  author: patch.author,
  version: patch.version,
  dependencies: patch.requires,
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
      version: patch.version,
      dependencies: patch.requires,
      description: patch.description,
      models: models(patch.models),
      packageId: patch.name,
      packageVersion: patch.version,
      readmeUrl: readmeUrl(patch),
      basedOn: effect.based_on,
      learnMore: effect.learn_more,
    })),
  )

/**
 * The hosted bundle for a room/spring assignment, as a path under
 * `public/loadable-patches/`. `l6patch prebuilt` names the bundles and lists
 * every one in the manifest, so the site never rebuilds those names.
 */
export async function effectReplacementFile(
  baseUrl: string,
  replacements: Record<string, string>,
): Promise<string> {
  const response = await fetch(`${baseUrl}loadable-patches/manifest.json`)
  if (!response.ok) throw new Error("Couldn't load the SD module list.")
  const manifest = (await response.json()) as {
    effect_replacements: { room: string; spring: string; file: string }[]
  }
  const room = replacements.room || 'stock'
  const spring = replacements.spring || 'stock'
  const entry = manifest.effect_replacements.find(
    (bundle) => bundle.room === room && bundle.spring === spring,
  )
  if (!entry) throw new Error(`No effect bundle for room ${room} and spring ${spring}.`)
  return entry.file
}
