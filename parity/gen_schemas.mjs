// Ekstrak TOOL_SCHEMAS efektif (registry ∩ skema) dari skills-engine Node
// menjadi JSON statis untuk yuki-rs (assets/tool_schemas.json).
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const here = path.dirname(fileURLToPath(import.meta.url))
const original = path.resolve(here, '../original')
process.chdir(original)
const engine = await import(path.join(original, 'lib/agent/skills-engine.js'))
await engine.initSkills(path.join(original, 'skills'))
const tools = await engine.getAvailableTools()
const skills = await engine.listSkills()
const out = path.resolve(here, '../yuki-rs/assets/tool_schemas.json')
fs.writeFileSync(out, JSON.stringify(tools, null, 1))
console.log(`tools=${tools.length} skills=${skills.length} -> ${out}`)
