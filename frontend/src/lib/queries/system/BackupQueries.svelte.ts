import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { backupsChanged, createAppMutation } from '$lib/queries/effects';
import { SYSTEM_ENDPOINTS } from './endpoints';
import { SystemQueryKeyFactory } from './SystemQueryKeyFactory';

export type BackupView = components['schemas']['BackupView'];
export type BackupRun = components['schemas']['BackupRunResponse'];
export type RestoreReport = components['schemas']['RestoreReport'];

/** The database backups on disk, oldest first, and how many are kept. Admin only. */
export const getBackupsQuery = () =>
	createQuery(() => ({
		queryKey: SystemQueryKeyFactory.backups(),
		queryFn: ({ signal }) => api.v3.GET(SYSTEM_ENDPOINTS.backups(), { signal })
	}));

/** Checks whether one backup would restore. Reads only; runs while `getName` names one. */
export const getRestoreReportQuery = (getName: Getter<string | null>) =>
	createQuery(() => {
		const name = getName();
		return {
			queryKey: SystemQueryKeyFactory.restoreReport(name ?? ''),
			queryFn: ({ signal }: { signal: AbortSignal }) =>
				api.v3.GET(SYSTEM_ENDPOINTS.restoreReport(name ?? ''), { signal }),
			enabled: name !== null,
			staleTime: 0
		};
	});

/** Backs up the database now; the oldest past the kept count is removed. */
export const createRunBackupMutation = () =>
	createAppMutation(() => ({
		mutationFn: () => api.v3.POST(SYSTEM_ENDPOINTS.backups()),
		effects: () => [backupsChanged()]
	}));
