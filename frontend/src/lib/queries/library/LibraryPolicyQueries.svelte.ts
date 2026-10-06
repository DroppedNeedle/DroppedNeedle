import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';
import { api } from '$lib/api/client';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import { LibraryV3Api } from './LibraryV3Api';
import { toTargetLibrarySettings } from './libraryAdapters';

export const getTargetLibrarySettingsQuery = (enabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		enabled: enabled(),
		queryKey: LibraryQueryKeyFactory.targetSettings(),
		queryFn: async ({ signal }) =>
			toTargetLibrarySettings(await api.global.v3.GET(LibraryV3Api.settings(), { signal }))
	}));

export const getLibraryRestorableRootsQuery = (enabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		enabled: enabled(),
		queryKey: LibraryQueryKeyFactory.restorableRoots(),
		queryFn: ({ signal }) => api.global.v3.GET(LibraryV3Api.restorableRoots(), { signal })
	}));

export const getLibraryPolicyTreeQuery = (enabled: Getter<boolean> = () => true) =>
	createQuery(() => ({
		enabled: enabled(),
		queryKey: LibraryQueryKeyFactory.policyTree(),
		queryFn: ({ signal }) => api.global.v3.GET(LibraryV3Api.policyTree(), { signal })
	}));

export const getLibraryPathMappingQuery = (enabled: Getter<boolean> = () => false) =>
	createQuery(() => ({
		enabled: enabled(),
		queryKey: LibraryQueryKeyFactory.pathMapping(),
		queryFn: ({ signal }) => api.global.v3.GET(LibraryV3Api.pathMapping(), { signal })
	}));
