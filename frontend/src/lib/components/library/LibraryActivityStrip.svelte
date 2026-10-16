<script lang="ts">
	import { ArrowRight, ChevronDown, X } from 'lucide-svelte';
	import { onMount } from 'svelte';

	import { authStore } from '$lib/stores/authStore.svelte';
	import { getLibraryActivityQuery } from '$lib/queries/library/LibraryActivityQueries.svelte';
	import type {
		LibraryActivityResponse,
		LibraryWorkItem
	} from '$lib/queries/library/LibraryOperationsTypes';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import LibraryWorkIcon from './LibraryWorkIcon.svelte';
	import LibraryWorkProgress from './LibraryWorkProgress.svelte';
	import {
		libraryWorkContext,
		libraryWorkEffect,
		libraryWorkHref,
		libraryWorkTitle
	} from './LibraryWorkPresentation';

	interface Props {
		activityOverride?: LibraryActivityResponse | null;
		now?: number;
		userIdOverride?: string;
		adminOverride?: boolean;
	}

	let {
		activityOverride = null,
		now = undefined,
		userIdOverride = undefined,
		adminOverride = undefined
	}: Props = $props();
	let currentTime = $state(Date.now() / 1000);
	let activityShell: HTMLDivElement | null = $state(null);
	let expanded = $state(false);
	let dismissedFailureKeys = $state<string[]>([]);
	const effectiveNow = $derived(now ?? currentTime);

	onMount(() => {
		if (now !== undefined) return;
		const timer = window.setInterval(() => {
			currentTime = Date.now() / 1000;
		}, 60_000);
		return () => window.clearInterval(timer);
	});

	$effect(() => {
		const shell = activityShell;
		if (!shell || typeof ResizeObserver === 'undefined') return;
		const container = shell.parentElement;
		if (!container) return;
		const updateOffset = () => {
			container.style.setProperty(
				'--droppedneedle-library-activity-height',
				`${shell.offsetHeight}px`
			);
		};
		const observer = new ResizeObserver(updateOffset);
		observer.observe(shell);
		updateOffset();
		return () => {
			observer.disconnect();
			container.style.removeProperty('--droppedneedle-library-activity-height');
		};
	});

	const userId = $derived(userIdOverride ?? authStore.user?.id);
	const isAdmin = $derived(adminOverride ?? authStore.isAdmin);
	const activityQuery = getLibraryActivityQuery(() => userId);
	const activity = $derived(activityOverride ?? activityQuery.data);
	const rawWorkItems = $derived(activity?.work_items ?? []);

	$effect(() => {
		const currentUserId = userId;
		const keys = rawWorkItems
			.map((item) => failureStorageKey(currentUserId, item))
			.filter((key): key is string => key !== null);
		if (typeof localStorage === 'undefined') {
			dismissedFailureKeys = [];
			return;
		}
		dismissedFailureKeys = keys.filter((key) => localStorage.getItem(key) === '1');
	});

	const workItems = $derived(
		rawWorkItems.filter((item) => {
			if (item.kind === 'recovery') return true;
			if (!item.failure_event_id || !item.failure_at) return true;
			if (effectiveNow - item.failure_at >= 24 * 60 * 60) return false;
			const key = failureStorageKey(userId, item);
			return key === null || !dismissedFailureKeys.includes(key);
		})
	);
	const primary = $derived(workItems[0] ?? null);
	const additional = $derived(workItems.slice(1));
	const destination = $derived(
		primary ? (isAdmin ? libraryWorkHref(primary) : '/library') : '/library'
	);
	const announcement = $derived(
		primary
			? `${libraryWorkTitle(primary)}. ${libraryWorkEffect(primary)}.`
			: 'No library work is running.'
	);

	$effect(() => {
		if (additional.length === 0) expanded = false;
	});

	function failureStorageKey(
		currentUserId: string | undefined,
		item: LibraryWorkItem
	): string | null {
		return currentUserId && item.failure_event_id
			? `droppedneedle:library-failure:${currentUserId}:${item.failure_event_id}`
			: null;
	}

	function dismissFailure(item: LibraryWorkItem): void {
		const key = failureStorageKey(userId, item);
		if (!key) return;
		dismissedFailureKeys = [...dismissedFailureKeys, key];
		try {
			localStorage.setItem(key, '1');
		} catch {
			// local dismissal still works when browser storage is unavailable
		}
	}
</script>

{#if primary}
	<div
		bind:this={activityShell}
		class="library-activity-shell"
		data-testid="library-activity-strip"
		data-effect={primary.effect}
	>
		<span class="sr-only" aria-live="polite" aria-atomic="true">{announcement}</span>
		<div class="library-activity-primary">
			<a href={destination} class="library-activity-primary__link">
				<span class="library-activity-primary__icon">
					<LibraryWorkIcon item={primary} />
				</span>
				<div class="min-w-0 flex-1">
					<div class="library-activity-primary__heading">
						<span>{libraryWorkEffect(primary)}</span>
						<strong>{libraryWorkTitle(primary)}</strong>
						{#if libraryWorkContext(primary)}<small>{libraryWorkContext(primary)}</small>{/if}
					</div>
					<LibraryWorkProgress item={primary} compact />
				</div>
				<ArrowRight class="h-4 w-4 shrink-0 text-base-content/45" aria-hidden="true" />
			</a>

			{#if primary.effect === 'attention' && primary.kind !== 'recovery'}
				<IconButton
					label="Dismiss library failure"
					icon={X}
					size="sm"
					shape="square"
					onclick={() => dismissFailure(primary)}
				/>
			{/if}
			{#if additional.length}
				<Button
					variant="ghost"
					size="xs"
					class="ml-1"
					aria-expanded={expanded}
					aria-controls="library-activity-work-stack"
					aria-label={`${expanded ? 'Hide' : 'Show'} ${additional.length} other ${additional.length === 1 ? 'task' : 'tasks'}`}
					onclick={() => (expanded = !expanded)}
				>
					<span class="max-sm:hidden"
						>+{additional.length} other {additional.length === 1 ? 'task' : 'tasks'}</span
					>
					<ChevronDown
						class={`h-4 w-4 transition-transform motion-reduce:transition-none${expanded ? ' rotate-180' : ''}`}
					/>
				</Button>
			{/if}
		</div>

		{#if expanded && additional.length}
			<div id="library-activity-work-stack" class="library-activity-stack">
				{#each additional as item (item.id)}
					<a
						href={isAdmin ? libraryWorkHref(item) : '/library'}
						class="library-activity-stack__item"
					>
						<span class="library-activity-stack__icon"><LibraryWorkIcon {item} /></span>
						<span class="min-w-0 flex-1">
							<strong>{libraryWorkTitle(item)}</strong>
							<small>{libraryWorkEffect(item)}</small>
							<LibraryWorkProgress {item} compact />
						</span>
						<ArrowRight class="h-4 w-4 shrink-0 text-base-content/40" aria-hidden="true" />
					</a>
				{/each}
			</div>
		{/if}
	</div>
{/if}
