/**
 * Template extraction for the contract coverage gate: fresh regexes per call
 * (module-level globals would carry lastIndex state between files).
 */

/** Templates spelled as string literals in builder calls. */
export function extractV3Templates(source: string): string[] {
	return [...source.matchAll(/\bv3\(\s*['"]([^'"]+)['"]/g)].map((match) => match[1] as string);
}

/** Every builder call, literal or dynamic (dynamic ones fail the gate). */
export function countV3Calls(source: string): number {
	return [...source.matchAll(/\bv3\(/g)].length;
}
