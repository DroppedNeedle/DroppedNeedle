import { SystemQueryKeyFactory } from '$lib/queries/featureKeys';

import { v3 } from '$lib/api/v3/endpoint';
import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { backupsChanged, createAppMutation } from '$lib/queries/effects';

export type BackupView = components['schemas']['BackupView'];
export type BackupRun = components['schemas']['BackupRunResponse'];
export type RestoreReport = components['schemas']['RestoreReport'];
export type ServiceHealthItem = components['schemas']['ServiceHealthItem'];

/** The database backups on disk, oldest first, and how many are kept. Admin only. */
export const getBackupsQuery = () =>
	createQuery(() => ({
		queryKey: SystemQueryKeyFactory.backups(),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/admin/backups'), { signal })
	}));

/** Checks whether one backup would restore. Reads only; runs while `getName` names one. */
export const getRestoreReportQuery = (getName: Getter<string | null>) =>
	createQuery(() => {
		const name = getName();
		return {
			queryKey: SystemQueryKeyFactory.restoreReport(name ?? ''),
			queryFn: ({ signal }: { signal: AbortSignal }) =>
				api.v3.GET(
					v3('/api/v3/admin/backups/{name}/restore-report', { path: { name: name ?? '' } }),
					{ signal }
				),
			enabled: name !== null,
			staleTime: 0
		};
	});

/** Backs up the database now; the oldest past the kept count is removed. */
export const createRunBackupMutation = () =>
	createAppMutation(() => ({
		mutationFn: () => api.v3.POST(v3('/api/v3/admin/backups')),
		effects: () => [backupsChanged()]
	}));

/** Which external services are currently degraded - drives the header health dot.
 * Polls on a slow cadence (a service outage is minutes-scale, not seconds) and on
 * window focus so a returning user sees current state. Not user-specific. */
export const getSystemHealthQuery = () =>
	createQuery(() => ({
		queryKey: SystemQueryKeyFactory.health(),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/system/health'), { signal }),
		refetchInterval: 60_000,
		refetchOnWindowFocus: true
	}));
