import { describe, expect, it } from 'vitest';
import { countV3Calls, extractV3Templates } from './registryScan';

describe('registry scan', () => {
	it('extracts single- and double-quoted templates', () => {
		const source = `a: () => v3('/api/v3/a'),\nb: () => v3("/api/v3/b/{id}", { path: { id } });`;

		expect(extractV3Templates(source)).toEqual(['/api/v3/a', '/api/v3/b/{id}']);
	});

	it('ignores the builder definition and typed client calls', () => {
		const source = `export function v3<const T>(t: T) {}\nawait api.v3.GET(ep);\nconst x = v3BaseUrl();`;

		expect(countV3Calls(source)).toBe(0);
		expect(extractV3Templates(source)).toEqual([]);
	});

	it('counts dynamic templates as calls but extracts nothing', () => {
		const source = `other: (t) => v3(t)`;

		expect(countV3Calls(source)).toBe(1);
		expect(extractV3Templates(source)).toEqual([]);
	});
});
