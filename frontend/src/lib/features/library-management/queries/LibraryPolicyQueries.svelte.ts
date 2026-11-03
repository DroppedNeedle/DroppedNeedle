import { v3 } from '$lib/api/v3/endpoint';
import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { api } from '$lib/api/client';
import { LibraryQueryKeyFactory } from '$lib/features/library/queries/LibraryQueryKeyFactory';

export const getTargetLibrarySettingsQuery = (enabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		enabled: enabled(),
		queryKey: LibraryQueryKeyFactory.targetSettings(),
		queryFn: async ({ signal }) => await api.v3.GET(v3('/api/v3/settings/library'), { signal })
	}));

export const getLibraryRestorableRootsQuery = (enabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		enabled: enabled(),
		queryKey: LibraryQueryKeyFactory.restorableRoots(),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/settings/library/restorable-roots'), { signal })
	}));

export const getLibraryPolicyTreeQuery = (enabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		enabled: enabled(),
		queryKey: LibraryQueryKeyFactory.policyTree(),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/settings/library/policy-tree'), { signal })
	}));
