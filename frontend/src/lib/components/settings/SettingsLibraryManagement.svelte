<script lang="ts">
	import { onMount, untrack } from 'svelte';
	import {
		ArchiveRestore,
		Check,
		ChevronRight,
		Clock3,
		CircleOff,
		Copy,
		FileUp,
		FolderCog,
		History,
		Link2,
		Pencil,
		RefreshCw,
		Share2,
		ShieldAlert,
		Square,
		Sparkles,
		Tags,
		Trash2
	} from 'lucide-svelte';

	import LibraryManagementProfilePicker from './LibraryManagementProfilePicker.svelte';
	import LibraryManagementProfileEditor from './LibraryManagementProfileEditor.svelte';
	import LibraryManagementProfileSharing from './LibraryManagementProfileSharing.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import {
		forgetLibraryManagementActivationSession,
		readLibraryManagementActivationSession,
		rememberLibraryManagementActivationSession,
		rememberLibraryManagementPreviewToken
	} from '$lib/queries/library-management/LibraryManagementPreviewTokens';
	import { createUuid } from '$lib/utils/uuid';
	import type { LibraryRootSettings } from '$lib/queries/library/LibraryOperationsTypes';
	import {
		getLibraryManagementActivationHealthQuery,
		getLibraryManagementActivationPreviewQuery,
		getLibraryManagementOperationsQuery,
		getLibraryManagementSettingsQuery
	} from '$lib/queries/library-management/LibraryManagementQueries.svelte';
	import {
		confirmLibraryManagementActivationMutation,
		controlLibraryManagementOperationMutation,
		copyLibraryManagementProfileMutation,
		createLibraryManagementActivationPreviewMutation,
		deleteLibraryManagementProfileMutation,
		previewLibraryManagementBaselinePurgeMutation,
		previewLibraryManagementSettingsImpactMutation,
		purgeLibraryManagementBaselinesMutation,
		updateLibraryManagementSettingsMutation,
		validateLibraryManagementSettingsMutation
	} from '$lib/queries/library-management/LibraryManagementMutations.svelte';
	import { LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE } from '$lib/queries/library-management/LibraryManagementConfirmation';
	import type {
		LibraryManagementActivationProof,
		LibraryManagementProfile,
		LibraryManagementProfileImportResponse,
		LibraryManagementRootAssignment,
		LibraryManagementRootOverrides,
		LibraryManagementSettings,
		LibraryManagementSettingsResponse,
		ManagementScriptSettings
	} from '$lib/queries/library-management/types';

	interface Props {
		roots: LibraryRootSettings[];
		policyRevision: string;
	}

	let { roots, policyRevision }: Props = $props();
	const settingsQuery = getLibraryManagementSettingsQuery(
		() => authStore.user?.id,
		() => authStore.isAdmin
	);
	const operationsQuery = getLibraryManagementOperationsQuery(
		() => authStore.user?.id,
		() => ({ limit: 20 })
	);
	const activationHealthQuery = getLibraryManagementActivationHealthQuery(
		() => authStore.user?.id,
		() => authStore.isAdmin
	);
	const updateSettings = updateLibraryManagementSettingsMutation();
	const validateSettings = validateLibraryManagementSettingsMutation();
	const impactSettings = previewLibraryManagementSettingsImpactMutation();
	const copyProfile = copyLibraryManagementProfileMutation();
	const deleteProfile = deleteLibraryManagementProfileMutation();
	const createActivation = createLibraryManagementActivationPreviewMutation();
	const confirmActivation = confirmLibraryManagementActivationMutation();
	const stopActivation = controlLibraryManagementOperationMutation('stop');
	const purgeImpact = previewLibraryManagementBaselinePurgeMutation();
	const purgeBaselines = purgeLibraryManagementBaselinesMutation();

	let draft = $state<LibraryManagementSettings | null>(null);
	let persistedSettings = $state<LibraryManagementSettings | null>(null);
	let sourceRevision = $state('');
	let selectedProfileId = $state<string | null>(null);
	let shareTarget = $state<LibraryManagementProfile | null>(null);
	let importProfileOpen = $state(false);
	let copySourceProfileId = $state('');
	let newProfileName = $state('Picard-style Organizer copy');
	let saveError = $state('');
	let managementHeading: HTMLHeadingElement;
	let activationDialog: HTMLDialogElement;
	let activationHeading: HTMLHeadingElement;
	let activationOpener: HTMLButtonElement | null = null;
	let activationDraft = $state<LibraryManagementSettings | null>(null);
	let activationRootIds = $state<string[]>([]);
	let activationIndex = $state(0);
	let activationJobId = $state<string | null>(null);
	let activationToken = $state('');
	let activationProofs = $state<LibraryManagementActivationProof[]>([]);
	let activationPhrase = $state('');
	let activationError = $state('');
	let activationSessionHydratedFor = $state<string | null>(null);
	let acceptingActivation = $state(false);
	let deleteDialog: HTMLDialogElement;
	let deleteHeading: HTMLHeadingElement;
	let deleteOpener: HTMLButtonElement | null = null;
	let deleteTarget = $state<LibraryManagementProfile | null>(null);
	let deleteError = $state('');
	let purgeDialog: HTMLDialogElement;
	let purgeHeading: HTMLHeadingElement;
	let purgeOpener: HTMLButtonElement | null = null;
	let purgePhrase = $state('');
	let purgeError = $state('');
	type BooleanOverrideKey =
		| 'metadata_enabled'
		| 'genres_enabled'
		| 'embedded_artwork_enabled'
		| 'external_artwork_enabled'
		| 'rename_enabled'
		| 'move_enabled'
		| 'move_sidecars'
		| 'preserve_timestamps';
	const booleanOverrides: Array<{ key: BooleanOverrideKey; label: string }> = [
		{ key: 'metadata_enabled', label: 'Metadata tags' },
		{ key: 'genres_enabled', label: 'Genres' },
		{ key: 'embedded_artwork_enabled', label: 'Embedded artwork' },
		{ key: 'external_artwork_enabled', label: 'External artwork' },
		{ key: 'rename_enabled', label: 'Rename files' },
		{ key: 'move_enabled', label: 'Organize within this root' },
		{ key: 'move_sidecars', label: 'Move sidecars' },
		{ key: 'preserve_timestamps', label: 'Preserve timestamps' }
	];

	const activationQuery = getLibraryManagementActivationPreviewQuery(
		() => authStore.user?.id,
		() => activationJobId
	);

	$effect(() => {
		const response = settingsQuery.data;
		const userId = authStore.user?.id ?? null;
		const hydratedFor = untrack(() => activationSessionHydratedFor);
		if (userId !== hydratedFor) {
			clearActivationState();
			activationSessionHydratedFor = null;
			if (!userId || !response) return;
			draft = settingsPayload(response);
			persistedSettings = settingsPayload(response);
			sourceRevision = response.settings_revision;
			const currentCopySource = untrack(() => copySourceProfileId);
			if (!response.profiles.some((profile) => profile.id === currentCopySource)) {
				copySourceProfileId = response.default_profile_id;
			}
			const session = readLibraryManagementActivationSession(userId);
			activationSessionHydratedFor = userId;
			if (
				session &&
				session.sourceRevision === response.settings_revision &&
				session.policyRevision === policyRevision
			) {
				draft = structuredClone(session.draft);
				activationDraft = structuredClone(session.activationDraft);
				activationRootIds = [...session.rootIds];
				activationIndex = session.rootIndex;
				activationJobId = session.jobId;
				activationToken = session.previewToken;
				activationProofs = structuredClone(session.proofs);
			} else {
				forgetLibraryManagementActivationSession(userId);
			}
			return;
		}
		const currentRevision = untrack(() => sourceRevision);
		if (response && response.settings_revision !== currentRevision) {
			draft = settingsPayload(response);
			persistedSettings = settingsPayload(response);
			sourceRevision = response.settings_revision;
			const currentCopySource = untrack(() => copySourceProfileId);
			if (!response.profiles.some((profile) => profile.id === currentCopySource)) {
				copySourceProfileId = response.default_profile_id;
			}
		}
	});

	$effect(() => {
		const userId = authStore.user?.id;
		if (!userId || activationSessionHydratedFor !== userId) return;
		if (!draft || !activationDraft || activationRootIds.length === 0) {
			forgetLibraryManagementActivationSession(userId);
			return;
		}
		rememberLibraryManagementActivationSession(userId, {
			sourceRevision,
			policyRevision,
			draft: $state.snapshot(draft),
			activationDraft: $state.snapshot(activationDraft),
			rootIds: $state.snapshot(activationRootIds),
			rootIndex: activationIndex,
			jobId: activationJobId,
			previewToken: activationToken,
			proofs: $state.snapshot(activationProofs)
		});
	});

	const profiles = $derived(draft?.profiles ?? []);
	const defaultProfile = $derived(
		profiles.find((profile) => profile.id === draft?.default_profile_id) ?? null
	);
	const activeAssignments = $derived(
		(persistedSettings?.root_assignments ?? []).filter(
			(assignment) =>
				assignment.enabled &&
				(assignment.automatic_acquisitions ||
					assignment.automatic_drop_imports ||
					assignment.automatic_scan_discovered)
		)
	);
	const operationHistory = $derived(
		operationsQuery.data?.pages.flatMap((page) => page.items) ?? []
	);
	const remoteActivation = $derived(
		operationHistory.find(
			(item) =>
				item.activation_preview &&
				['queued', 'running', 'paused', 'ready'].includes(item.operation.state)
		) ?? null
	);
	const hasUnsavedSettings = $derived(
		Boolean(
			draft &&
			persistedSettings &&
			JSON.stringify($state.snapshot(draft)) !== JSON.stringify($state.snapshot(persistedSettings))
		)
	);
	const selectedProfile = $derived(
		profiles.find((profile) => profile.id === selectedProfileId) ?? null
	);
	const staleActivationRootIds = $derived(
		activationHealthQuery.isLoading ? [] : (activationHealthQuery.data?.stale_root_ids ?? [])
	);
	const staleActivationRootLabels = $derived(
		staleActivationRootIds.map(
			(rootId) => roots.find((root) => root.id === rootId)?.label ?? 'Unknown root'
		)
	);
	const blockedActivationRootIds = $derived(
		activationHealthQuery.isLoading ? [] : (activationHealthQuery.data?.blocked_root_ids ?? [])
	);
	const blockedActivationRootLabels = $derived(
		blockedActivationRootIds.map(
			(rootId) => roots.find((root) => root.id === rootId)?.label ?? 'Unknown root'
		)
	);
	const blockedActivationReason = $derived(
		activationHealthQuery.isLoading ? null : (activationHealthQuery.data?.blocked_reason ?? null)
	);
	const currentActivationRootId = $derived(activationRootIds[activationIndex] ?? null);
	const currentActivationRoot = $derived(
		roots.find((root) => root.id === currentActivationRootId) ?? null
	);
	const activationReady = $derived(
		Boolean(
			activationQuery.data?.ready_for_confirmation &&
			!activationQuery.data.expired &&
			!activationQuery.data.stale
		)
	);
	const activationStalled = $derived(Boolean(activationQuery.data?.worker_stalled));
	const activationCoversCurrentDraft = $derived.by(() => {
		if (!activationDraft || !draft) return true;
		return (
			JSON.stringify($state.snapshot(activationDraft)) === JSON.stringify($state.snapshot(draft))
		);
	});
	const activationPending = $derived(
		createActivation.isPending ||
			confirmActivation.isPending ||
			stopActivation.isPending ||
			acceptingActivation
	);
	const activationCanStop = $derived(
		Boolean(
			activationQuery.data &&
			['queued', 'running', 'paused'].includes(activationQuery.data.state) &&
			activationQuery.data.control_request !== 'stop'
		)
	);

	function settingsPayload(response: LibraryManagementSettingsResponse): LibraryManagementSettings {
		const value = structuredClone(response);
		const { settings_revision: _revision, ...settings } = value;
		return settings;
	}

	function emptyAssignment(rootId: string): LibraryManagementRootAssignment {
		return {
			root_id: rootId,
			profile_id: null,
			overrides: null,
			enabled: false,
			automatic_acquisitions: false,
			automatic_drop_imports: false,
			automatic_scan_discovered: false,
			automatic_custom_editions: false,
			activation_profile_revision: null,
			activation_naming_policy_revision: null,
			activation_policy_revision: null,
			activation_settings_revision: null,
			activation_preview_token: null,
			activation_preview_hash: null,
			activation_confirmed_at: null
		};
	}

	function emptyOverrides(): LibraryManagementRootOverrides {
		return {
			metadata_enabled: null,
			genres_enabled: null,
			embedded_artwork_enabled: null,
			external_artwork_enabled: null,
			rename_enabled: null,
			move_enabled: null,
			move_sidecars: null,
			source_cleanup: null,
			preserve_timestamps: null,
			naming_script_id: null,
			multi_disc_naming_mode: 'inherit',
			multi_disc_naming_script_id: null,
			automatic_edition_acceptance_enabled: null
		};
	}

	function assignmentFor(rootId: string): LibraryManagementRootAssignment {
		return (
			draft?.root_assignments.find((assignment) => assignment.root_id === rootId) ??
			emptyAssignment(rootId)
		);
	}

	function persistedAssignmentFor(rootId: string): LibraryManagementRootAssignment {
		return (
			persistedSettings?.root_assignments.find((assignment) => assignment.root_id === rootId) ??
			emptyAssignment(rootId)
		);
	}

	function rootManagementStatus(rootId: string): string {
		const saved = persistedAssignmentFor(rootId);
		const current = assignmentFor(rootId);
		if (JSON.stringify($state.snapshot(current)) !== JSON.stringify($state.snapshot(saved))) {
			return 'Pending unsaved changes';
		}
		if (
			saved.enabled &&
			(saved.automatic_acquisitions ||
				saved.automatic_drop_imports ||
				saved.automatic_scan_discovered)
		) {
			if (activationHealthQuery.isLoading) return 'Checking activation…';
			if (blockedActivationRootIds.includes(rootId)) return 'Paused — needs attention';
			return staleActivationRootIds.includes(rootId)
				? 'Activation stale — run a dry run'
				: 'Active';
		}
		return saved.enabled ? 'Configured; automatic triggers off' : 'Off';
	}

	function updateAssignment(
		rootId: string,
		update: (assignment: LibraryManagementRootAssignment) => LibraryManagementRootAssignment
	): void {
		if (!draft) return;
		const current = assignmentFor(rootId);
		const next = update($state.snapshot(current));
		const exists = draft.root_assignments.some((assignment) => assignment.root_id === rootId);
		draft = {
			...draft,
			root_assignments: exists
				? draft.root_assignments.map((assignment) =>
						assignment.root_id === rootId ? next : assignment
					)
				: [...draft.root_assignments, next]
		};
	}

	function updateDefaultProfile(profileId: string): void {
		if (!draft) return;
		draft = { ...draft, default_profile_id: profileId };
	}

	function updateOverrides(rootId: string, update: Partial<LibraryManagementRootOverrides>): void {
		updateAssignment(rootId, (assignment) => ({
			...assignment,
			overrides: { ...(assignment.overrides ?? emptyOverrides()), ...update }
		}));
	}

	function nullableBoolean(value: string): boolean | null {
		return value === '' ? null : value === 'true';
	}

	function assignedRootCount(profileId: string): number {
		if (!draft) return 0;
		return roots.filter((root) => {
			const assignment = draft?.root_assignments.find((value) => value.root_id === root.id);
			return Boolean(
				assignment && (assignment.profile_id ?? draft?.default_profile_id) === profileId
			);
		}).length;
	}

	function assignedRootLabels(profileId: string): string[] {
		if (!draft) return [];
		return roots.flatMap((root) => {
			const assignment = draft?.root_assignments.find((value) => value.root_id === root.id);
			return assignment && (assignment.profile_id ?? draft?.default_profile_id) === profileId
				? [root.label]
				: [];
		});
	}

	function profileDeleteReason(profile: LibraryManagementProfile): string | null {
		if (!draft || profile.preset_origin) return null;
		if (profiles.length === 1) return 'Only remaining profile';
		if (profile.id === draft.default_profile_id) return 'Library default';
		if (assignedRootCount(profile.id) > 0) return 'Assigned to root';
		return null;
	}

	function profileAspects(profile: LibraryManagementProfile): string[] {
		const aspects: string[] = [];
		if (profile.metadata.enabled) aspects.push('tags');
		if (profile.genres.enabled) aspects.push('genres');
		if (profile.artwork.embedded_enabled || profile.artwork.external_enabled)
			aspects.push('artwork');
		if (profile.enrichment.lyrics.enabled) aspects.push('lyrics');
		if (profile.enrichment.replaygain.enabled) aspects.push('ReplayGain');
		if (profile.organization.rename_enabled) aspects.push('rename');
		if (profile.organization.move_enabled) aspects.push('move');
		return aspects;
	}

	function upsertProfile(
		current: LibraryManagementProfile[],
		profile: LibraryManagementProfile
	): LibraryManagementProfile[] {
		const saved = structuredClone(profile);
		return current.some((value) => value.id === saved.id)
			? current.map((value) => (value.id === saved.id ? saved : value))
			: [...current, saved];
	}

	function upsertScripts(
		current: ManagementScriptSettings[],
		created: ManagementScriptSettings[]
	): ManagementScriptSettings[] {
		const createdIds = new Set(created.map((script) => script.id));
		return [...current.filter((script) => !createdIds.has(script.id)), ...structuredClone(created)];
	}

	function acceptImportedProfile(result: LibraryManagementProfileImportResponse): void {
		if (!draft) return;
		draft = {
			...draft,
			profiles: upsertProfile(draft.profiles, result.profile),
			naming_scripts: upsertScripts(draft.naming_scripts, result.naming_scripts),
			tagging_scripts: upsertScripts(draft.tagging_scripts, result.tagging_scripts)
		};
		if (persistedSettings) {
			persistedSettings = {
				...persistedSettings,
				profiles: upsertProfile(persistedSettings.profiles, result.profile),
				naming_scripts: upsertScripts(persistedSettings.naming_scripts, result.naming_scripts),
				tagging_scripts: upsertScripts(persistedSettings.tagging_scripts, result.tagging_scripts)
			};
		}
		sourceRevision = result.settings_revision;
		copySourceProfileId = result.profile.id;
		selectedProfileId = result.profile.id;
	}

	function activationProfileFor(rootId: string): LibraryManagementProfile | null {
		if (!activationDraft) return null;
		const assignment = activationDraft.root_assignments.find((value) => value.root_id === rootId);
		const profileId = assignment?.profile_id ?? activationDraft.default_profile_id;
		return activationDraft.profiles.find((profile) => profile.id === profileId) ?? null;
	}

	async function reviewAndSave(
		proposed: LibraryManagementSettings,
		opener: HTMLButtonElement | null = null
	): Promise<void> {
		saveError = '';
		try {
			await validateSettings.mutateAsync({
				settings: proposed,
				expected_settings_revision: sourceRevision
			});
			const impact = await impactSettings.mutateAsync({
				settings: proposed,
				expected_settings_revision: sourceRevision
			});
			if (impact.stale) throw new Error('Organization settings changed. Reload and try again.');
			const activeAffected = impact.affected_root_ids.filter((rootId) => {
				const assignment = proposed.root_assignments.find((value) => value.root_id === rootId);
				return Boolean(
					assignment?.enabled &&
					(assignment.automatic_acquisitions ||
						assignment.automatic_drop_imports ||
						assignment.automatic_scan_discovered)
				);
			});
			if (impact.preview_required && activeAffected.length > 0) {
				const resumesCurrentActivation = Boolean(
					activationDraft &&
					JSON.stringify($state.snapshot(activationDraft)) === JSON.stringify(proposed) &&
					JSON.stringify($state.snapshot(activationRootIds)) === JSON.stringify(activeAffected)
				);
				if (!resumesCurrentActivation) {
					activationDraft = structuredClone(proposed);
					activationRootIds = activeAffected;
					activationIndex = 0;
					activationJobId = null;
					activationToken = '';
					activationProofs = [];
				}
				activationPhrase = '';
				activationError = '';
				showActivationDialog(opener);
				return;
			}
			const saved = await updateSettings.mutateAsync({
				settings: proposed,
				expected_settings_revision: sourceRevision
			});
			draft = settingsPayload(saved);
			persistedSettings = settingsPayload(saved);
			sourceRevision = saved.settings_revision;
			resetActivationSession();
		} catch (error) {
			saveError = error instanceof Error ? error.message : 'Could not save organization settings.';
			throw error;
		}
	}

	async function saveProfile(
		profile: LibraryManagementProfile,
		namingScripts: ManagementScriptSettings[],
		taggingScripts: ManagementScriptSettings[]
	): Promise<void> {
		if (!draft) return;
		const proposed = {
			...$state.snapshot(draft),
			profiles: draft.profiles.map((value) => (value.id === profile.id ? profile : value)),
			naming_scripts: namingScripts,
			tagging_scripts: taggingScripts
		};
		await reviewAndSave(proposed);
		selectedProfileId = null;
	}

	async function createProfileCopy(): Promise<void> {
		if (!draft || !copySourceProfileId || !newProfileName.trim()) return;
		saveError = '';
		try {
			const result = await copyProfile.mutateAsync({
				profileId: copySourceProfileId,
				request: { name: newProfileName.trim(), expected_settings_revision: sourceRevision }
			});
			draft = { ...draft, profiles: upsertProfile(draft.profiles, result.profile) };
			if (persistedSettings) {
				persistedSettings = {
					...persistedSettings,
					profiles: upsertProfile(persistedSettings.profiles, result.profile)
				};
			}
			sourceRevision = result.settings_revision;
			selectedProfileId = result.profile.id;
		} catch (error) {
			saveError = error instanceof Error ? error.message : 'Could not create the profile.';
		}
	}

	function requestDelete(profile: LibraryManagementProfile, opener: HTMLButtonElement): void {
		deleteTarget = profile;
		deleteError = '';
		deleteOpener = opener;
		deleteDialog.showModal();
		deleteHeading.focus();
	}

	async function confirmDelete(): Promise<void> {
		if (!deleteTarget) return;
		const deletedProfileId = deleteTarget.id;
		try {
			const saved = await deleteProfile.mutateAsync({
				profileId: deleteTarget.id,
				request: { expected_settings_revision: sourceRevision }
			});
			draft = settingsPayload(saved);
			persistedSettings = settingsPayload(saved);
			sourceRevision = saved.settings_revision;
			if (copySourceProfileId === deletedProfileId) {
				copySourceProfileId = saved.default_profile_id;
			}
			deleteDialog.close();
			deleteTarget = null;
		} catch (error) {
			deleteError = error instanceof Error ? error.message : 'Could not delete the profile.';
		}
	}

	async function runActivationPreview(): Promise<void> {
		if (!activationDraft || !currentActivationRootId) return;
		activationError = '';
		try {
			const handle = await createActivation.mutateAsync({
				root_id: currentActivationRootId,
				settings: activationDraft,
				expected_settings_revision: sourceRevision,
				expected_policy_revision: policyRevision,
				idempotency_key: createUuid()
			});
			activationJobId = handle.job_id;
			activationToken = handle.preview_token;
			rememberLibraryManagementPreviewToken(handle.job_id, handle.preview_token);
		} catch (error) {
			activationError =
				error instanceof Error ? error.message : 'Could not start the activation dry run.';
		}
	}

	async function acceptActivationPreview(): Promise<void> {
		if (!activationReady || !currentActivationRootId || !activationJobId) return;
		activationError = '';
		acceptingActivation = true;
		try {
			const refreshed = await activationQuery.refetch();
			if (refreshed.error) throw refreshed.error;
			const preview = refreshed.data;
			if (!preview || !preview.ready_for_confirmation || preview.stale || preview.expired) {
				activationError =
					'This dry run is no longer current. Start a fresh dry run before enabling automatic writes.';
				return;
			}
		} catch (error) {
			activationError = error instanceof Error ? error.message : 'Could not recheck this dry run.';
			return;
		} finally {
			acceptingActivation = false;
		}
		activationProofs = [
			...activationProofs,
			{
				root_id: currentActivationRootId,
				job_id: activationJobId,
				preview_token: activationToken
			}
		];
		activationIndex += 1;
		activationJobId = null;
		activationToken = '';
	}

	function discardActivationPreview(): void {
		if (activationPending) return;
		activationJobId = null;
		activationToken = '';
		activationError = '';
	}

	async function stopActivationPreview(): Promise<void> {
		const preview = activationQuery.data;
		if (!activationJobId || !preview || !activationCanStop) return;
		activationError = '';
		try {
			await stopActivation.mutateAsync({
				jobId: activationJobId,
				expectedRevision: preview.operation_row_revision
			});
			await activationQuery.refetch();
		} catch (error) {
			activationError = error instanceof Error ? error.message : 'Could not stop this dry run.';
		}
	}

	async function restartActivationReview(opener: HTMLButtonElement): Promise<void> {
		if (!draft || activationPending) return;
		if (activationCanStop) {
			await stopActivationPreview();
			if (activationError) return;
		}
		const proposed = $state.snapshot(draft);
		resetActivationSession();
		await reviewAndSave(proposed, opener).catch(() => undefined);
	}

	function startFreshActivation(opener: HTMLButtonElement): void {
		if (
			!persistedSettings ||
			hasUnsavedSettings ||
			activationPending ||
			staleActivationRootIds.length === 0
		)
			return;
		activationDraft = structuredClone($state.snapshot(persistedSettings));
		activationRootIds = [...staleActivationRootIds];
		activationIndex = 0;
		activationJobId = null;
		activationToken = '';
		activationProofs = [];
		activationPhrase = '';
		activationError = '';
		showActivationDialog(opener);
	}

	function showActivationDialog(opener: HTMLButtonElement | null): void {
		activationPhrase = '';
		activationOpener = opener;
		activationDialog.showModal();
		activationHeading.focus();
	}

	function resetActivationSession(): void {
		const userId = authStore.user?.id;
		if (userId) forgetLibraryManagementActivationSession(userId);
		clearActivationState();
	}

	function clearActivationState(): void {
		activationDraft = null;
		activationRootIds = [];
		activationIndex = 0;
		activationJobId = null;
		activationToken = '';
		activationProofs = [];
		activationPhrase = '';
		activationError = '';
	}

	async function enableManagement(): Promise<void> {
		if (
			!activationDraft ||
			activationProofs.length !== activationRootIds.length ||
			activationPhrase !== LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE ||
			!activationCoversCurrentDraft
		)
			return;
		activationError = '';
		try {
			const saved = await confirmActivation.mutateAsync({
				settings: activationDraft,
				proofs: activationProofs,
				expected_settings_revision: sourceRevision,
				confirmation: true
			});
			draft = settingsPayload(saved);
			persistedSettings = settingsPayload(saved);
			sourceRevision = saved.settings_revision;
			resetActivationSession();
			activationDialog.close();
		} catch (error) {
			activationError =
				error instanceof Error ? error.message : 'Could not enable file organization.';
		}
	}

	function restoreActivationFocus(): void {
		if (activationOpener?.isConnected) {
			activationOpener.focus();
			return;
		}
		managementHeading.focus();
	}

	function restoreDeleteFocus(): void {
		if (deleteOpener?.isConnected) {
			deleteOpener.focus();
			return;
		}
		managementHeading.focus();
	}

	async function openPurge(opener: HTMLButtonElement): Promise<void> {
		purgePhrase = '';
		purgeError = '';
		purgeOpener = opener;
		try {
			await purgeImpact.mutateAsync();
			purgeDialog.showModal();
			purgeHeading.focus();
		} catch (error) {
			saveError = error instanceof Error ? error.message : 'Could not inspect baseline usage.';
		}
	}

	async function confirmPurge(): Promise<void> {
		const impact = purgeImpact.data;
		if (!impact || purgePhrase !== LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE) return;
		try {
			await purgeBaselines.mutateAsync({
				impact_token: impact.impact_token,
				expected_catalog_revision: impact.catalog_revision,
				typed_confirmation: purgePhrase,
				idempotency_key: createUuid()
			});
			purgeDialog.close();
		} catch (error) {
			purgeError = error instanceof Error ? error.message : 'Could not purge the baselines.';
		}
	}

	function saveDraft(event: MouseEvent & { currentTarget: HTMLButtonElement }): void {
		if (!draft) return;
		void reviewAndSave($state.snapshot(draft), event.currentTarget).catch(() => undefined);
	}

	onMount(() => {
		if (!authStore.isAdmin) selectedProfileId = null;
	});
</script>

<section class="management-settings-shell" aria-labelledby="library-management-title">
	<header class="management-settings-header">
		<div class="management-write-mark"><FolderCog class="h-6 w-6" /></div>
		<div class="min-w-0 flex-1">
			<p class="management-kicker"><ShieldAlert class="h-3.5 w-3.5" /> Optional write access</p>
			<h2
				bind:this={managementHeading}
				id="library-management-title"
				tabindex="-1"
				class="font-display text-xl font-semibold"
			>
				Automation
			</h2>
			<p class="mt-1 text-sm text-base-content/65">
				Writes tags and moves files. It is separate from scanning and remains off until an
				administrator confirms a dry run.
			</p>
		</div>
		<span class="management-status-badge" data-active={activeAssignments.length > 0}>
			{#if activeAssignments.length > 0}<Check class="h-3.5 w-3.5" /> Active on {activeAssignments.length}
				root{activeAssignments.length === 1 ? '' : 's'}{:else}<CircleOff class="h-3.5 w-3.5" /> Off everywhere{/if}
		</span>
	</header>

	{#if settingsQuery.isLoading}
		<div class="space-y-3 p-5">
			<div class="skeleton h-28 rounded-xl"></div>
			<div class="skeleton h-48 rounded-xl"></div>
		</div>
	{:else if settingsQuery.isError || !draft}
		<div class="m-5 alert alert-error">Could not load organization settings.</div>
	{:else}
		<div class="space-y-6 p-5 sm:p-6">
			<section class="space-y-3" aria-labelledby="management-profiles-title">
				<div>
					<div>
						<p class="management-step">01 · Profiles</p>
						<h3 id="management-profiles-title" class="font-display text-lg font-semibold">
							Choose what "managed" means
						</h3>
						<p class="text-xs text-base-content/55">
							Choose one library default below. Profiles do nothing until you assign one to a root.
						</p>
					</div>
				</div>

				<!-- svelte-ignore a11y_no_noninteractive_tabindex (bounded scroll region must be keyboard-focusable) -->
				<div
					class="management-profile-grid"
					role="region"
					aria-label="Saved organization profiles"
					tabindex="0"
				>
					{#each profiles as profile (profile.id)}
						<article
							class="management-profile-card"
							data-default={profile.id === draft.default_profile_id}
							aria-labelledby={`management-profile-${profile.id}`}
						>
							<div class="flex items-start justify-between gap-3">
								<div class="min-w-0">
									<div class="flex flex-wrap items-center gap-2">
										<h4 id={`management-profile-${profile.id}`} class="font-semibold">
											{profile.name}
										</h4>
										<span
											class="badge badge-xs {profile.preset_origin
												? 'badge-outline'
												: 'badge-ghost'}">{profile.preset_origin ? 'Preset' : 'Custom'}</span
										>
									</div>
									<p class="mt-1 line-clamp-2 text-xs text-base-content/55">
										{profile.description || 'No description.'}
									</p>
								</div>
								<Tags class="h-5 w-5 shrink-0 text-library-manage" />
							</div>
							<div class="mt-3 flex flex-wrap gap-1.5">
								{#each profileAspects(profile) as aspect (aspect)}<span class="management-aspect"
										>{aspect}</span
									>{/each}
							</div>
							<div class="management-profile-card__footer">
								<div class="management-profile-card__state">
									<label
										class="management-profile-default-control"
										data-unsaved={profile.id === draft.default_profile_id &&
											draft.default_profile_id !== persistedSettings?.default_profile_id}
									>
										<input
											type="radio"
											name="library-default-profile"
											class="radio radio-xs"
											checked={profile.id === draft.default_profile_id}
											aria-label={`Make ${profile.name} the library default`}
											onchange={() => updateDefaultProfile(profile.id)}
										/>
										<span
											>{profile.id !== draft.default_profile_id
												? 'Make default'
												: draft.default_profile_id !== persistedSettings?.default_profile_id
													? 'Default after save'
													: 'Library default'}</span
										>
									</label>
									<span
										class="text-xs font-medium text-base-content/55"
										title={assignedRootLabels(profile.id).length
											? `Assigned to: ${assignedRootLabels(profile.id).join(', ')}`
											: undefined}
										>{#if assignedRootLabels(profile.id).length}<Link2
												class="mr-1 inline h-3.5 w-3.5 align-text-bottom text-library-manage"
											/>Assigned to {assignedRootLabels(profile.id).join(', ')}{:else}Not assigned
											to a root{/if}</span
									>
								</div>
								<div class="management-profile-card__actions">
									<button
										class="btn btn-ghost btn-xs"
										aria-label={`Share ${profile.name}`}
										onclick={() => (shareTarget = profile)}
										><Share2 class="h-3.5 w-3.5" /> Share</button
									>
									<button
										class="btn btn-ghost btn-xs"
										onclick={() => (selectedProfileId = profile.id)}
										><Pencil class="h-3.5 w-3.5" /> Edit</button
									>
									{#if !profile.preset_origin}
										{@const deleteReason = profileDeleteReason(profile)}
										<button
											class="btn btn-ghost btn-xs {deleteReason
												? 'text-base-content/45'
												: 'btn-square text-error'}"
											aria-label={deleteReason
												? `Cannot delete ${profile.name}: ${deleteReason}`
												: `Delete ${profile.name}`}
											disabled={Boolean(deleteReason)}
											onclick={(event) => requestDelete(profile, event.currentTarget)}
											><Trash2 class="h-3.5 w-3.5" />{#if deleteReason}<span>{deleteReason}</span
												>{/if}</button
										>
									{/if}
								</div>
							</div>
						</article>
					{/each}
				</div>

				<div class="management-create-row">
					<div>
						<strong class="text-sm">Copy an existing profile</strong>
						<p class="text-xs text-base-content/50">
							Copies every saved value without changing root assignments or enabling automation.
						</p>
					</div>
					<select
						class="select select-bordered select-sm min-w-52 bg-base-100"
						bind:value={copySourceProfileId}
						aria-label="Profile to copy"
					>
						{#each profiles as profile (profile.id)}<option value={profile.id}
								>{profile.name}{profile.preset_origin ? ' · preset based' : ' · custom'}</option
							>{/each}
					</select>
					<input
						class="input input-bordered input-sm min-w-52 bg-base-100"
						bind:value={newProfileName}
						aria-label="New profile name"
					/>
					<button
						class="btn btn-outline btn-sm"
						disabled={copyProfile.isPending || !copySourceProfileId || !newProfileName.trim()}
						onclick={() => void createProfileCopy()}><Copy class="h-4 w-4" /> Create copy</button
					>
					<button
						class="btn btn-outline btn-sm"
						disabled={hasUnsavedSettings}
						title={hasUnsavedSettings
							? 'Save or discard current organization changes before importing.'
							: undefined}
						onclick={() => (importProfileOpen = true)}
						><FileUp class="h-4 w-4" /> Import profile</button
					>
				</div>
				{#if hasUnsavedSettings}<p class="text-xs text-warning">
						Save or discard current organization changes before importing a profile.
					</p>{/if}
			</section>

			<section class="space-y-3" aria-labelledby="management-roots-title">
				<div>
					<p class="management-step">02 · Root automation</p>
					<h3 id="management-roots-title" class="font-display text-lg font-semibold">
						Choose where writes are allowed
					</h3>
					<p class="text-xs text-base-content/55">
						Scanning reads and identifies. These controls separately allow future writes in each
						root.
					</p>
				</div>
				<div class="space-y-3">
					{#each roots as root (root.id)}
						{@const assignment = assignmentFor(root.id)}
						<article class="management-root-card">
							<div class="flex flex-wrap items-start justify-between gap-3">
								<div>
									<div class="flex flex-wrap items-center gap-2">
										<h4 class="font-display text-lg font-semibold">{root.label}</h4>
										<span class="badge badge-ghost badge-sm"
											>Scanning: {root.policy === 'automatic'
												? 'Automatic identification'
												: root.policy.replace('_', ' ')}</span
										>
									</div>
									<p class="mt-1 font-mono text-xs text-base-content/40">{root.path}</p>
								</div>
								<span class="management-status-badge" data-active={assignment.enabled}
									>{rootManagementStatus(root.id)}</span
								>
							</div>
							<div class="mt-4">
								<LibraryManagementProfilePicker
									id={`root-${root.id}`}
									eyebrow={assignment.profile_id
										? 'Explicit root profile'
										: 'Inherited from library default'}
									label={`Management profile for ${root.label}`}
									description="Defines tags, artwork, enrichment, and file organization for this root. Choose the library default or make an explicit override."
									{profiles}
									selectedId={assignment.profile_id ?? ''}
									changed={assignment.profile_id !== persistedAssignmentFor(root.id).profile_id}
									inheritedProfile={defaultProfile}
									onselect={(profileId) =>
										updateAssignment(root.id, (value) => ({
											...value,
											profile_id: profileId || null
										}))}
								/>
							</div>
							<div class="mt-4 border-t border-base-content/10 pt-4">
								<label class="management-master-toggle max-w-xl"
									><input
										type="checkbox"
										class="toggle toggle-sm"
										checked={assignment.enabled}
										onchange={(event) =>
											updateAssignment(root.id, (value) => ({
												...value,
												enabled: event.currentTarget.checked
											}))}
									/><span
										><strong>Configure file organization for this root</strong><small
											>This alone starts nothing. Choose and confirm automatic triggers below.</small
										></span
									></label
								>
								<div
									class="mt-3 grid gap-2 md:grid-cols-3"
									aria-label={`Automatic triggers for ${root.label}`}
								>
									<label class="management-trigger"
										><input
											type="checkbox"
											class="checkbox checkbox-sm"
											disabled={!assignment.enabled}
											checked={assignment.automatic_acquisitions}
											onchange={(event) =>
												updateAssignment(root.id, (value) => ({
													...value,
													automatic_acquisitions: event.currentTarget.checked
												}))}
										/><span
											><strong>Acquisitions</strong><small
												>Applies to new Soulseek and Usenet downloads.</small
											></span
										></label
									>
									<label class="management-trigger"
										><input
											type="checkbox"
											class="checkbox checkbox-sm"
											disabled={!assignment.enabled}
											checked={assignment.automatic_drop_imports}
											onchange={(event) =>
												updateAssignment(root.id, (value) => ({
													...value,
													automatic_drop_imports: event.currentTarget.checked
												}))}
										/><span
											><strong>Drop & Free imports</strong><small
												>Only identified, mapped files.</small
											></span
										></label
									>
									<label class="management-trigger"
										><input
											type="checkbox"
											class="checkbox checkbox-sm"
											disabled={!assignment.enabled}
											checked={assignment.automatic_scan_discovered}
											onchange={(event) =>
												updateAssignment(root.id, (value) => ({
													...value,
													automatic_scan_discovered: event.currentTarget.checked,
													automatic_custom_editions: event.currentTarget.checked
														? value.automatic_custom_editions
														: false
												}))}
										/><span
											><strong>Scan-discovered</strong><small
												>Off by default. Accepted release and track identity required.</small
											></span
										></label
									>
									<label class="management-trigger pl-6"
										><input
											type="checkbox"
											class="checkbox checkbox-sm"
											disabled={!assignment.enabled || !assignment.automatic_scan_discovered}
											checked={assignment.automatic_custom_editions}
											onchange={(event) =>
												updateAssignment(root.id, (value) => ({
													...value,
													automatic_custom_editions: event.currentTarget.checked
												}))}
										/><span
											><strong>Include Custom editions</strong><small
												>Requires a current sealed manifest and a fresh dry run.</small
											></span
										></label
									>
								</div>
								<details class="mt-3 rounded-xl border border-base-content/10 p-3">
									<summary class="cursor-pointer text-xs font-semibold"
										>Per-root profile overrides</summary
									>
									<label class="management-master-toggle mt-3 max-w-xl">
										<input
											type="checkbox"
											class="toggle toggle-sm"
											checked={assignment.overrides !== null}
											onchange={(event) =>
												updateAssignment(root.id, (value) => ({
													...value,
													overrides: event.currentTarget.checked ? emptyOverrides() : null
												}))}
										/>
										<span
											><strong>Override selected profile values</strong><small
												>Every untouched control continues to inherit the profile.</small
											></span
										>
									</label>
									{#if assignment.overrides}
										<div class="mt-3 grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
											{#each booleanOverrides as control (control.key)}
												<label class="grid gap-1 text-xs"
													><span>{control.label}</span><select
														class="select select-bordered select-sm bg-base-100"
														value={assignment.overrides[control.key] === null
															? ''
															: String(assignment.overrides[control.key])}
														onchange={(event) =>
															updateOverrides(root.id, {
																[control.key]: nullableBoolean(event.currentTarget.value)
															})}
														><option value="">Inherit profile</option><option value="true"
															>On</option
														><option value="false">Off</option></select
													></label
												>
											{/each}
											<label class="grid gap-1 text-xs"
												><span>Naming script</span><select
													class="select select-bordered select-sm bg-base-100"
													value={assignment.overrides.naming_script_id ?? ''}
													onchange={(event) =>
														updateOverrides(root.id, {
															naming_script_id: event.currentTarget.value || null
														})}
													><option value="">Inherit profile</option
													>{#each draft.naming_scripts as script (script.id)}<option
															value={script.id}>{script.name}</option
														>{/each}</select
												></label
											>
											<label class="grid gap-1 text-xs">
												<span>Multi-disc naming</span>
												<select
													class="select select-bordered select-sm bg-base-100"
													value={assignment.overrides.multi_disc_naming_mode}
													onchange={(event) => {
														const mode = event.currentTarget.value as
															| 'inherit'
															| 'standard'
															| 'script';
														updateOverrides(root.id, {
															multi_disc_naming_mode: mode,
															multi_disc_naming_script_id:
																mode === 'script'
																	? (assignment.overrides?.multi_disc_naming_script_id ??
																		draft?.naming_scripts[0]?.id ??
																		null)
																	: null
														});
													}}
												>
													<option value="inherit">Inherit profile</option>
													<option value="standard">Use effective single-disc script</option>
													<option value="script">Use selected script</option>
												</select>
											</label>
											{#if assignment.overrides.multi_disc_naming_mode === 'script'}
												<label class="grid gap-1 text-xs">
													<span>Selected multi-disc script</span>
													<select
														class="select select-bordered select-sm bg-base-100"
														value={assignment.overrides.multi_disc_naming_script_id ?? ''}
														onchange={(event) =>
															updateOverrides(root.id, {
																multi_disc_naming_script_id: event.currentTarget.value || null
															})}
													>
														{#each draft.naming_scripts as script (script.id)}
															<option value={script.id}>{script.name}</option>
														{/each}
													</select>
												</label>
											{/if}
											<label class="grid gap-1 text-xs"
												><span>Source cleanup</span><select
													class="select select-bordered select-sm bg-base-100"
													value={assignment.overrides.source_cleanup ?? ''}
													onchange={(event) =>
														updateOverrides(root.id, {
															source_cleanup:
																event.currentTarget.value === 'keep' ||
																event.currentTarget.value === 'remove_after_confirmed_move'
																	? event.currentTarget.value
																	: null
														})}
													><option value="">Inherit profile</option><option value="keep"
														>Keep source</option
													><option value="remove_after_confirmed_move"
														>Remove after verified move</option
													></select
												></label
											>
										</div>
									{/if}
								</details>
							</div>
						</article>
					{/each}
				</div>
			</section>

			<details class="management-retention-panel">
				<summary
					><span class="management-editor-icon"><History class="h-4 w-4" /></span><span
						><strong>Retention, recycle, and refresh</strong><small
							>Advanced recovery settings</small
						></span
					><ChevronRight class="ml-auto h-4 w-4 management-editor-chevron" /></summary
				>
				<div class="mt-4 grid gap-4 sm:grid-cols-2">
					<label class="grid gap-1.5 text-sm"
						><span>Undo snapshots (days)</span><input
							type="number"
							min="1"
							max="3650"
							class="input input-bordered bg-base-100"
							bind:value={draft.undo_retention_days}
						/></label
					>
					<label class="grid gap-1.5 text-sm"
						><span>Preview lifetime (hours)</span><input
							type="number"
							min="1"
							max="168"
							class="input input-bordered bg-base-100"
							bind:value={draft.preview_retention_hours}
						/></label
					>
					<label class="grid gap-1.5 text-sm sm:col-span-2"
						><span>Recycle directory</span><input
							class="input input-bordered bg-base-100 font-mono text-sm"
							bind:value={draft.recycle_bin_path}
							placeholder="/srv/music-recycle"
						/><small class="text-base-content/50"
							>A recovery destination, not a scanned library root. Recycle actions stay unavailable
							while empty or unsafe.</small
						></label
					>
					<label class="management-master-toggle"
						><input
							type="checkbox"
							class="toggle toggle-sm"
							bind:checked={draft.external_refresh.enabled}
						/><span
							><strong>Post-commit refresh</strong><small
								>Refresh DroppedNeedle and selected media servers.</small
							></span
						></label
					>
					{#if draft.external_refresh.enabled}
						<div class="management-choice-grid sm:col-span-2" aria-label="External refresh targets">
							<label
								><input
									type="checkbox"
									class="checkbox checkbox-xs"
									bind:checked={draft.external_refresh.plex_enabled}
								/><span>Plex</span></label
							>
							<label
								><input
									type="checkbox"
									class="checkbox checkbox-xs"
									bind:checked={draft.external_refresh.jellyfin_enabled}
								/><span>Jellyfin</span></label
							>
							<label
								><input
									type="checkbox"
									class="checkbox checkbox-xs"
									bind:checked={draft.external_refresh.navidrome_enabled}
								/><span>Navidrome</span></label
							>
						</div>
						<label class="grid gap-1.5 text-sm"
							><span>Refresh retry attempts</span><input
								type="number"
								min="0"
								max="20"
								class="input input-bordered bg-base-100"
								bind:value={draft.external_refresh.retry_attempts}
							/></label
						>
						<label class="grid gap-1.5 text-sm"
							><span>Retry delay (seconds)</span><input
								type="number"
								min="0"
								max="3600"
								class="input input-bordered bg-base-100"
								bind:value={draft.external_refresh.retry_delay_seconds}
							/></label
						>
					{/if}
					<div class="rounded-xl border border-error/20 bg-error/5 p-3 sm:col-span-2">
						<div class="flex flex-wrap items-center justify-between gap-3">
							<div>
								<strong class="text-sm">Original baselines</strong>
								<p class="text-xs text-base-content/55">
									Independent of Undo. Purging permanently removes the oldest restoration point.
								</p>
							</div>
							<button
								class="btn btn-error btn-outline btn-sm"
								disabled={purgeImpact.isPending}
								onclick={(event) => void openPurge(event.currentTarget)}
								><ArchiveRestore class="h-4 w-4" /> Purge baselines...</button
							>
						</div>
					</div>
				</div>
			</details>

			{#if saveError}<div class="alert alert-error text-sm" role="alert">{saveError}</div>{/if}
			<div class="management-save-bar" aria-live="polite">
				{#if activationDraft && activationRootIds.length > 0}
					<div>
						<strong class="text-sm">
							{#if !activationCoversCurrentDraft}Configuration changed after this dry run started{:else if !currentActivationRoot}Activation
								awaits final confirmation{:else if activationReady}Dry run ready for review{:else if activationQuery.data?.control_request === 'stop'}Dry
								run stopping{:else if activationStalled}Dry run interrupted{:else if activationQuery.data?.state === 'queued'}Dry
								run queued{:else if activationQuery.data?.state === 'running'}Dry run planning{:else if ['failed', 'stopped', 'cancelled'].includes(activationQuery.data?.state ?? '')}Dry
								run needs attention{:else}Activation dry run continues{/if}
						</strong>
						<p class="text-xs text-base-content/50">
							{#if !activationCoversCurrentDraft}The existing result cannot authorize the settings
								now shown. Restart the review to create one current dry run.{:else if currentActivationRoot}{currentActivationRoot.label}
								{#if (activationQuery.data?.summary.item_count ?? 0) > 0}
									· {(activationQuery.data?.summary.item_count ?? 0).toLocaleString()}
									files found · {(activationQuery.data?.summary.bundle_count ?? 0).toLocaleString()}
									release bundles{:else}
									· Discovering files and release bundles{/if}{:else}Every required dry run is
								accepted. Automatic writes are still off.{/if}
						</p>
					</div>
					{#if !activationCoversCurrentDraft}<button
							class="btn management-btn"
							disabled={activationPending}
							onclick={(event) => void restartActivationReview(event.currentTarget)}
							><RefreshCw class="h-4 w-4" /> Restart review</button
						>
					{:else}<button
							class="btn management-btn"
							onclick={(event) => showActivationDialog(event.currentTarget)}
							>{#if currentActivationRoot && activationQuery.data?.state === 'running' && !activationStalled}<span
									class="loading loading-spinner loading-sm"
								></span>{/if}<ShieldAlert class="h-4 w-4" /> View dry run</button
						>
					{/if}
				{:else if hasUnsavedSettings}
					<div>
						<strong class="text-sm">Review configuration changes</strong>
						<p class="text-xs text-base-content/50">
							Trigger-only changes save immediately. Changes to file-writing scope require a current
							dry run.
						</p>
					</div>
					<button
						class="btn management-btn"
						disabled={updateSettings.isPending ||
							validateSettings.isPending ||
							impactSettings.isPending}
						onclick={saveDraft}
						>{#if updateSettings.isPending || validateSettings.isPending || impactSettings.isPending}<span
								class="loading loading-spinner loading-sm"
							></span>{/if}<Sparkles class="h-4 w-4" /> Validate and save</button
					>
				{:else if remoteActivation}
					<div>
						<strong class="text-sm">
							{remoteActivation.operation.state === 'ready'
								? 'Write-access dry run ready'
								: 'Write-access dry run in progress'}
						</strong>
						<p class="text-xs text-base-content/50">
							{roots.find((root) => root.id === remoteActivation.target_root_id)?.label ??
								'This root'} · {remoteActivation.profile_name} · {remoteActivation.operation
								.state === 'ready'
								? 'Ready to review'
								: 'Discovering files and release bundles'}
						</p>
					</div>
					<a
						class="btn management-btn"
						href={`/library/management/previews/${encodeURIComponent(remoteActivation.operation.id)}`}
						><ShieldAlert class="h-4 w-4" /> Review dry run</a
					>
				{:else if activationHealthQuery.isError}
					<div>
						<strong class="text-sm">Could not check activation status</strong>
						<p class="text-xs text-base-content/50">
							Automatic writes may be paused. Retry the status check.
						</p>
					</div>
					<button
						class="btn management-btn"
						disabled={activationHealthQuery.isFetching}
						onclick={() => void activationHealthQuery.refetch()}
						><RefreshCw class="h-4 w-4" /> Retry status check</button
					>
				{:else if activationHealthQuery.isLoading}
					<div>
						<strong class="text-sm">Checking activation status…</strong>
						<p class="text-xs text-base-content/50">
							Confirming every automatic root still matches its dry run.
						</p>
					</div>
					<button class="btn btn-ghost" disabled
						><span class="loading loading-spinner loading-sm"></span> Checking</button
					>
				{:else if blockedActivationRootIds.length > 0}
					<div>
						<strong class="text-sm">Automatic writes paused — needs attention</strong>
						<p class="text-xs text-base-content/50">
							{#if blockedActivationRootLabels.length === 1}
								{blockedActivationRootLabels[0]} needs attention before automatic writes can
								resume. Check the library roots and the assignment below, then recheck.
							{:else}
								{blockedActivationRootLabels.length} roots need attention before automatic
								writes can resume. Check the library roots and assignments below, then recheck.
							{/if}
						</p>
						{#if blockedActivationReason}
							<p class="mt-1 text-xs text-base-content/50">{blockedActivationReason}</p>
						{/if}
					</div>
					<button
						class="btn management-btn"
						disabled={activationHealthQuery.isFetching}
						onclick={() => void activationHealthQuery.refetch()}
						><RefreshCw class="h-4 w-4" /> Recheck</button
					>
				{:else if staleActivationRootIds.length > 0}
					<div>
						<strong class="text-sm">Automatic writes paused — dry run needed</strong>
						<p class="text-xs text-base-content/50">
							{#if staleActivationRootLabels.length === 1}
								{staleActivationRootLabels[0]} no longer matches its confirmed dry run. Run a fresh dry
								run to resume automatic writes.
							{:else}
								{staleActivationRootLabels.length} roots no longer match their confirmed dry run. Run
								a fresh dry run to resume automatic writes.
							{/if}
						</p>
					</div>
					<button
						class="btn management-btn"
						disabled={activationPending || activationHealthQuery.isFetching}
						onclick={(event) => startFreshActivation(event.currentTarget)}
						><ShieldAlert class="h-4 w-4" /> Run a fresh dry run</button
					>
				{:else}
					<div>
						<strong class="text-sm">Configuration saved</strong>
						<p class="text-xs text-base-content/50">
							Change a profile, assignment, or automatic trigger to review an update.
						</p>
					</div>
					<button class="btn btn-ghost" disabled><Check class="h-4 w-4" /> Saved</button>
				{/if}
			</div>
		</div>
	{/if}
</section>

{#if selectedProfile}
	<LibraryManagementProfileEditor
		profile={selectedProfile}
		namingScripts={draft?.naming_scripts ?? []}
		taggingScripts={draft?.tagging_scripts ?? []}
		saving={updateSettings.isPending || validateSettings.isPending || impactSettings.isPending}
		onclose={() => (selectedProfileId = null)}
		onsave={saveProfile}
	/>
{/if}

<LibraryManagementProfileSharing
	shareProfile={shareTarget}
	importOpen={importProfileOpen}
	settingsRevision={sourceRevision}
	onshareclose={() => (shareTarget = null)}
	onimportclose={() => (importProfileOpen = false)}
	onimported={acceptImportedProfile}
/>

<dialog
	bind:this={activationDialog}
	class="modal"
	aria-labelledby="management-activation-title"
	onclose={restoreActivationFocus}
	oncancel={(event) => {
		if (activationPending) event.preventDefault();
	}}
>
	<div class="modal-box max-w-2xl management-activation-dialog">
		<p class="management-kicker"><ShieldAlert class="h-3.5 w-3.5" /> Write-access gate</p>
		<h2
			bind:this={activationHeading}
			id="management-activation-title"
			tabindex="-1"
			class="font-display text-xl font-semibold"
		>
			Enable file organization
		</h2>
		<p class="mt-2 text-sm text-base-content/65">
			Nothing is enabled until a current dry run exists for every affected root and you confirm the
			exact phrase.
		</p>
		<div class="mt-5 flex flex-wrap gap-2" aria-label="Activation progress">
			{#each activationRootIds as rootId, index (rootId)}{@const root = roots.find(
					(value) => value.id === rootId
				)}<span
					class="management-activation-step"
					data-state={index < activationIndex
						? 'done'
						: index === activationIndex
							? 'current'
							: 'waiting'}
					>{#if index < activationIndex}<Check class="h-3.5 w-3.5" />{/if}{root?.label ??
						rootId}</span
				>{/each}
		</div>
		{#if currentActivationRoot}
			{@const activationProfile = activationProfileFor(currentActivationRoot.id)}
			<section class="mt-5 rounded-xl border border-base-content/10 bg-base-100/65 p-4">
				<h3 class="font-semibold">Dry run · {currentActivationRoot.label}</h3>
				<p class="mt-1 text-xs text-base-content/55">
					Previews the selected profile against this root. It reads files and writes only the saved
					preview.
				</p>
				{#if activationProfile}
					<div
						class="mt-3 rounded-lg border border-library-manage/20 bg-library-manage/5 p-3 text-xs"
					>
						<strong>{activationProfile.name}</strong>
						<div class="mt-2 flex flex-wrap gap-1.5">
							{#each profileAspects(activationProfile) as aspect (aspect)}<span
									class="management-aspect">{aspect}</span
								>{/each}
						</div>
						<p class="mt-2 text-base-content/55">
							Organization remains inside this root. Only files with an accepted MusicBrainz release
							and per-track mapping are eligible.
						</p>
					</div>
				{/if}
				{#if !activationJobId}
					<button
						class="btn management-btn btn-sm mt-4"
						disabled={createActivation.isPending}
						onclick={() => void runActivationPreview()}
						>{#if createActivation.isPending}<span class="loading loading-spinner loading-sm"
							></span>{/if}<RefreshCw class="h-4 w-4" /> Run dry run</button
					>
				{:else if activationQuery.isLoading}
					<div class="mt-4 skeleton h-20 rounded-xl"></div>
				{:else if activationQuery.isError}
					<div class="alert alert-error mt-4 items-start text-sm" role="alert">
						<ShieldAlert class="mt-0.5 h-5 w-5" /><span
							><strong>Could not load this dry run.</strong><br />Retry the status request or run a
							fresh dry run before enabling anything.</span
						>
					</div>
					<button
						class="btn btn-outline btn-sm mt-3"
						disabled={activationQuery.isFetching}
						onclick={() => void activationQuery.refetch()}>Retry status</button
					>
					<button
						class="btn btn-ghost btn-sm mt-3"
						disabled={activationPending}
						onclick={discardActivationPreview}>Start a fresh dry run</button
					>
				{:else if activationQuery.data}
					{#if !activationReady && (activationQuery.data.summary.item_count ?? 0) > 0}<div
							class="mt-4 flex flex-wrap items-baseline justify-between gap-x-5 gap-y-1 rounded-lg border border-library-manage/20 bg-library-manage/5 px-3.5 py-3"
							aria-live="polite"
						>
							<span class="text-xs font-semibold uppercase tracking-[0.16em] text-base-content/45"
								>Planning progress</span
							>
							<span class="font-semibold text-base-content/80"
								>{(activationQuery.data.summary.item_count ?? 0).toLocaleString()} files</span
							>
							<span class="text-sm text-base-content/55"
								>{(activationQuery.data.summary.bundle_count ?? 0).toLocaleString()} release bundles</span
							>
						</div>{:else if !activationReady}<div
							class="mt-4 flex items-center gap-3 rounded-lg border border-library-manage/20 bg-library-manage/5 px-3.5 py-3 text-sm text-base-content/60"
							aria-live="polite"
						>
							<span
								class="loading loading-spinner loading-sm text-library-manage"
								aria-hidden="true"
							></span><span
								><strong class="block text-base-content/80">Discovering the root</strong>File and
								release totals appear after the first planning checkpoint.</span
							>
						</div>{/if}
					{#if activationReady}<div class="mt-4 grid grid-cols-2 gap-2 sm:grid-cols-4">
							<div class="management-mini-stat">
								<span>Eligible</span><strong>{activationQuery.data.summary.eligible_count}</strong>
							</div>
							<div class="management-mini-stat">
								<span>Warnings</span><strong>{activationQuery.data.summary.warning_count}</strong>
							</div>
							<div class="management-mini-stat">
								<span>Blocked</span><strong>{activationQuery.data.summary.blocked_count}</strong>
							</div>
							<div class="management-mini-stat">
								<span>Moves</span><strong>{activationQuery.data.summary.path_change_count}</strong>
							</div>
							<div class="management-mini-stat">
								<span>Tag writes</span><strong
									>{activationQuery.data.summary.tag_change_count ?? 0}</strong
								>
							</div>
							<div class="management-mini-stat">
								<span>Artwork</span><strong
									>{activationQuery.data.summary.artwork_change_count ?? 0}</strong
								>
							</div>
							<div class="management-mini-stat">
								<span>Sidecars</span><strong
									>{activationQuery.data.summary.sidecar_change_count ?? 0}</strong
								>
							</div>
							<div class="management-mini-stat">
								<span>No change</span><strong
									>{activationQuery.data.summary.no_change_count ?? 0}</strong
								>
							</div>
						</div>{/if}
					<a
						href={`/library/management/previews/${encodeURIComponent(activationJobId)}`}
						target="_blank"
						rel="noopener noreferrer"
						class="btn btn-ghost btn-sm mt-3">Review file-by-file dry run</a
					>
					{#if activationQuery.data.stale || activationQuery.data.expired}<div
							class="alert alert-error mt-3 text-sm"
						>
							This dry run is stale or expired. Run it again before enabling anything.
						</div>
						<button class="btn btn-outline btn-sm mt-3" onclick={discardActivationPreview}
							>Run a fresh dry run</button
						>
					{:else if ['failed', 'stopped', 'cancelled'].includes(activationQuery.data.state)}<div
							class="alert alert-error mt-3 text-sm"
							role="alert"
						>
							{activationQuery.data.state === 'failed'
								? 'This dry run failed during planning.'
								: 'This dry run was stopped.'} No files were changed.
						</div>
						<button class="btn btn-outline btn-sm mt-3" onclick={discardActivationPreview}
							>Run a fresh dry run</button
						>
					{:else if activationQuery.data.control_request === 'stop'}<div
							class="mt-3 flex items-center gap-3 rounded-lg border border-warning/20 bg-warning/5 px-3 py-2.5 text-sm text-base-content/65"
							role="status"
							aria-live="polite"
						>
							<span class="loading loading-spinner loading-sm text-warning" aria-hidden="true"
							></span><span
								><strong class="block text-base-content/80">Stopping this dry run</strong>The
								planner will stop at its next safe checkpoint. No music files are involved.</span
							>
						</div>
					{:else if activationStalled}<div
							class="alert alert-error mt-3 items-start text-sm"
							role="alert"
						>
							<ShieldAlert class="mt-0.5 h-5 w-5 shrink-0" aria-hidden="true" /><span
								><strong class="block">Planning stopped responding</strong>The last saved checkpoint
								contains {(activationQuery.data.summary.item_count ?? 0).toLocaleString()}
								files. No music files changed. Stop this dry run, then start a fresh one; planning resumes
								from a new operation.</span
							>
						</div>
					{:else if activationQuery.data.state === 'queued'}<div
							class="mt-3 flex items-center gap-3 rounded-lg border border-base-content/10 bg-base-200/45 px-3 py-2.5 text-sm text-base-content/60"
							role="status"
							aria-live="polite"
						>
							<Clock3 class="h-5 w-5 shrink-0 text-library-manage" aria-hidden="true" /><span
								><strong class="block text-base-content/75">Queued for planning</strong>It starts
								automatically when the current library operation finishes. You can safely close this
								dialog.</span
							>
						</div>
					{:else if !activationReady}<div
							class="mt-3 flex items-center gap-3 rounded-lg border border-base-content/10 bg-base-200/45 px-3 py-2.5 text-sm text-base-content/60"
							role="status"
							aria-live="polite"
						>
							<span
								class="loading loading-spinner loading-sm text-library-manage"
								aria-hidden="true"
							></span><span
								><strong class="block text-base-content/75">Planning this dry run</strong>This
								updates automatically. You can safely close the dialog while it continues.</span
							>
						</div>{/if}
					<button
						class="btn management-btn btn-sm mt-4"
						disabled={!activationReady || activationPending}
						onclick={() => void acceptActivationPreview()}
						>{#if acceptingActivation}<span class="loading loading-spinner loading-sm"
							></span>{/if}Use this dry run <ChevronRight class="h-4 w-4" /></button
					>
				{/if}
			</section>
		{:else}
			<section class="mt-5 space-y-3">
				<div class="alert alert-warning items-start">
					<ShieldAlert class="mt-0.5 h-5 w-5" /><span
						>Automatic management will write tags and may move files according to the previewed
						profiles. Closing this dialog leaves the saved configuration unchanged.</span
					>
				</div>
				<label class="grid gap-1.5 text-sm"
					><span>Type <strong>{LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE}</strong></span><input
						class="input input-bordered bg-base-100"
						bind:value={activationPhrase}
						autocomplete="off"
					/></label
				>
			</section>
		{/if}
		{#if activationError}<div class="alert alert-error mt-4 text-sm" role="alert">
				{activationError}
			</div>{/if}
		<div class="modal-action">
			{#if currentActivationRoot && activationCanStop}<button
					class="btn btn-error btn-outline mr-auto"
					disabled={activationPending}
					onclick={() => void stopActivationPreview()}
					>{#if stopActivation.isPending}<span class="loading loading-spinner loading-sm"
						></span>{/if}<Square class="h-4 w-4" /> Stop dry run</button
				>
			{/if}
			<button
				class="btn btn-ghost"
				onclick={() => activationDialog.close()}
				disabled={activationPending}>Close</button
			>{#if !currentActivationRoot}<button
					class="btn management-btn"
					disabled={activationPhrase !== LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE ||
						activationPending ||
						!activationCoversCurrentDraft}
					onclick={() => void enableManagement()}
					>{#if confirmActivation.isPending}<span class="loading loading-spinner loading-sm"
						></span>{/if} Enable file organization</button
				>{/if}
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close organization activation" disabled={activationPending}>close</button>
	</form>
</dialog>

<dialog
	bind:this={deleteDialog}
	class="modal"
	aria-labelledby="delete-management-profile-title"
	onclose={restoreDeleteFocus}
	oncancel={(event) => {
		if (deleteProfile.isPending) event.preventDefault();
	}}
>
	<div class="modal-box max-w-md">
		<h2
			bind:this={deleteHeading}
			id="delete-management-profile-title"
			tabindex="-1"
			class="text-lg font-bold"
		>
			Delete {deleteTarget?.name ?? 'profile'}?
		</h2>
		<p class="mt-3 text-sm text-base-content/65">
			This removes only the profile. Assigned profiles cannot be deleted, and no music file is
			touched.
		</p>
		{#if deleteError}<div class="alert alert-error mt-3 text-sm" role="alert">
				{deleteError}
			</div>{/if}
		<div class="modal-action">
			<button
				class="btn btn-ghost"
				disabled={deleteProfile.isPending}
				onclick={() => deleteDialog.close()}>Cancel</button
			><button
				class="btn btn-error"
				disabled={deleteProfile.isPending}
				onclick={() => void confirmDelete()}
				>{#if deleteProfile.isPending}<span class="loading loading-spinner loading-sm"></span>{/if} Delete
				profile</button
			>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Cancel profile deletion" disabled={deleteProfile.isPending}>close</button>
	</form>
</dialog>

<dialog
	bind:this={purgeDialog}
	class="modal"
	aria-labelledby="purge-management-baselines-title"
	onclose={() => purgeOpener?.focus()}
	oncancel={(event) => {
		if (purgeBaselines.isPending) event.preventDefault();
	}}
>
	<div class="modal-box max-w-lg">
		<h2
			bind:this={purgeHeading}
			id="purge-management-baselines-title"
			tabindex="-1"
			class="text-lg font-bold"
		>
			Purge original baselines?
		</h2>
		{#if purgeImpact.data}<div class="mt-3 space-y-3 text-sm">
				<p>
					This permanently removes <strong
						>{purgeImpact.data.baseline_count.toLocaleString()} baselines</strong
					>
					and may clean {purgeImpact.data.referenced_blob_count.toLocaleString()} unreferenced snapshot
					files.
				</p>
				{#if purgeImpact.data.blocked_journal_count || purgeImpact.data.active_restore_count}<div
						class="alert alert-error"
					>
						Recovery or restore work is active. Purge is blocked.
					</div>{/if}<label class="grid gap-1.5"
					><span>Type <strong>{LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE}</strong></span><input
						class="input input-bordered bg-base-100"
						bind:value={purgePhrase}
						autocomplete="off"
					/></label
				>
			</div>{/if}
		{#if purgeError}<div class="alert alert-error mt-3 text-sm" role="alert">
				{purgeError}
			</div>{/if}
		<div class="modal-action">
			<button
				class="btn btn-ghost"
				disabled={purgeBaselines.isPending}
				onclick={() => purgeDialog.close()}>Cancel</button
			><button
				class="btn btn-error"
				disabled={purgePhrase !== LIBRARY_MANAGEMENT_CONFIRMATION_PHRASE ||
					purgeBaselines.isPending ||
					Boolean(
						purgeImpact.data?.blocked_journal_count || purgeImpact.data?.active_restore_count
					)}
				onclick={() => void confirmPurge()}
				>{#if purgeBaselines.isPending}<span class="loading loading-spinner loading-sm"></span>{/if} Purge
				baselines</button
			>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Cancel baseline purge" disabled={purgeBaselines.isPending}>close</button>
	</form>
</dialog>
