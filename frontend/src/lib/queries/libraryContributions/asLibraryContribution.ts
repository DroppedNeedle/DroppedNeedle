import type { components } from '$lib/api/v3/openapi';
import type { LibraryContribution } from '$lib/types';

// The contract marks the record's defaulted fields optional, but the server
// always sends every field, so the screens keep the stricter shape.
export const asLibraryContribution = (
	record: components['schemas']['ContributionRecord']
): LibraryContribution => record as LibraryContribution;
