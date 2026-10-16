<script lang="ts">
	import {
		getUserQuotaQuery,
		saveUserQuota,
		type UserQuotaResponse
	} from '$lib/queries/auth/UserQuotaQueries.svelte';
	import { toastStore } from '$lib/stores/toast';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import Field from '$lib/ui/Field.svelte';
	import Input from '$lib/ui/Input.svelte';

	interface Props {
		userId: string;
		displayName: string;
	}

	let { userId, displayName }: Props = $props();

	const quotaQuery = getUserQuotaQuery(
		() => userId,
		() => true
	);
	const save = saveUserQuota();

	// blank input = inherit the global default (sent as null); a number input's
	// binding yields number | null once edited, string only from our seeding
	let requestCount = $state<string | number | null>('');
	let requestDays = $state<string | number | null>('');
	let storageGb = $state<string | number | null>('');
	let seeded = $state(false);

	$effect(() => {
		const d = quotaQuery.data;
		if (d && !seeded) {
			requestCount = d.quota_override.request_quota_count?.toString() ?? '';
			requestDays = d.quota_override.request_quota_days?.toString() ?? '';
			storageGb = d.quota_override.storage_quota_gb?.toString() ?? '';
			seeded = true;
		}
	});

	function parsed(value: string | number | null): number | null {
		if (value === null || value === '') return null;
		const n = typeof value === 'number' ? value : Number(String(value).trim());
		return Number.isFinite(n) && n >= 0 ? Math.floor(n) : null;
	}

	function usedPercent(used: number, cap: number): number {
		return cap > 0 ? Math.min(100, (used / cap) * 100) : 0;
	}

	function storageGbUsed(d: UserQuotaResponse): number {
		return d.storage_bytes / 1024 ** 3;
	}

	async function handleSave() {
		try {
			await save.mutateAsync({
				userId,
				override: {
					request_quota_count: parsed(requestCount),
					request_quota_days: parsed(requestDays),
					storage_quota_gb: parsed(storageGb)
				}
			});
			toastStore.show({ message: `Quota saved for ${displayName}`, type: 'success' });
		} catch {
			toastStore.show({ message: 'Could not save that quota', type: 'error' });
		}
	}
</script>

<Card variant="bordered" padding="sm">
	{#if quotaQuery.isPending}
		<div class="skeleton h-16 w-full"></div>
	{:else if quotaQuery.isError}
		<p class="text-xs text-error">Could not load this user's quota.</p>
	{:else if quotaQuery.data}
		{@const d = quotaQuery.data}
		{#if d.exempt}
			<p class="text-xs text-base-content/60">
				Admin and trusted accounts are exempt from per-user quotas (the global storage cap still
				applies).
			</p>
		{:else}
			<div class="grid gap-3 sm:grid-cols-2">
				<div class="space-y-1">
					<p class="text-xs text-base-content/60">
						Requests: {d.requests_in_window}
						{#if d.effective_request_quota_count > 0}
							of {d.effective_request_quota_count} per {d.effective_request_quota_days} days
						{:else}
							in the last {d.effective_request_quota_days} days (no limit)
						{/if}
					</p>
					{#if d.effective_request_quota_count > 0}
						<progress
							class="progress w-full {usedPercent(
								d.requests_in_window,
								d.effective_request_quota_count
							) >= 100
								? 'progress-error'
								: 'progress-primary'}"
							value={usedPercent(d.requests_in_window, d.effective_request_quota_count)}
							max="100"
						></progress>
					{/if}
				</div>
				<div class="space-y-1">
					<p class="text-xs text-base-content/60">
						Downloads: {storageGbUsed(d).toFixed(1)} GB
						{#if d.effective_storage_quota_gb > 0}
							of {d.effective_storage_quota_gb} GB
						{:else}
							(no limit)
						{/if}
					</p>
					{#if d.effective_storage_quota_gb > 0}
						<progress
							class="progress w-full {usedPercent(storageGbUsed(d), d.effective_storage_quota_gb) >=
							100
								? 'progress-error'
								: 'progress-primary'}"
							value={usedPercent(storageGbUsed(d), d.effective_storage_quota_gb)}
							max="100"
						></progress>
					{/if}
				</div>
			</div>
		{/if}

		<div class="flex flex-wrap items-end gap-2">
			<Field label="Requests">
				{#snippet children(control)}
					<Input
						{...control}
						label="Requests"
						type="number"
						size="sm"
						min="0"
						class="w-24"
						placeholder="default"
						bind:value={requestCount}
					/>
				{/snippet}
			</Field>
			<Field label="Window (days)">
				{#snippet children(control)}
					<Input
						{...control}
						label="Window (days)"
						type="number"
						size="sm"
						min="1"
						class="w-24"
						placeholder="default"
						bind:value={requestDays}
					/>
				{/snippet}
			</Field>
			<Field label="Storage (GB)">
				{#snippet children(control)}
					<Input
						{...control}
						label="Storage (GB)"
						type="number"
						size="sm"
						min="0"
						class="w-28"
						placeholder="default"
						bind:value={storageGb}
					/>
				{/snippet}
			</Field>
			<Button
				variant="primary"
				size="sm"
				class="mb-1"
				loading={save.isPending}
				onclick={handleSave}
			>
				Save
			</Button>
		</div>
		<p class="text-[11px] text-base-content/40">Blank = use the global default. 0 = unlimited.</p>
	{/if}
</Card>
