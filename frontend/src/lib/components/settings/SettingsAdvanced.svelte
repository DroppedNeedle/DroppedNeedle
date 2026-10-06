<script lang="ts">
	import { SETTINGS_ENDPOINTS } from '$lib/queries/settings/endpoints';
	import {
		CircleCheck,
		CircleAlert,
		Clock,
		Archive,
		Globe,
		Database,
		RotateCcw,
		Save,
		ImageIcon
	} from 'lucide-svelte';
	import { createSettingsForm } from '$lib/utils/settingsForm.svelte';
	import { onDestroy } from 'svelte';
	import type { AdvancedSettingsForm } from './advanced-settings-types';
	import SettingsSectionCollapse from './SettingsSectionCollapse.svelte';
	import SettingsFrontendCache from './SettingsFrontendCache.svelte';
	import SettingsBackendCache from './SettingsBackendCache.svelte';
	import SettingsNetworkBatch from './SettingsNetworkBatch.svelte';
	import SettingsStorageQueue from './SettingsStorageQueue.svelte';
	import SettingsAudioDB from './SettingsAudioDB.svelte';

	const form = createSettingsForm<AdvancedSettingsForm>({
		loadEndpoint: SETTINGS_ENDPOINTS.advanced(),
		saveEndpoint: SETTINGS_ENDPOINTS.advanced(),
		afterSave: () => {
			form.showMessage('Settings saved. Some changes take effect after a page reload');
		}
	});

	export async function load() {
		await form.load();
	}

	async function save() {
		await form.save();
	}

	$effect(() => {
		load();
	});

	let openFrontendCache = $state(true);
	let openBackendCache = $state(false);
	let openNetworkBatch = $state(false);
	let openStorageQueue = $state(false);
	let openAudioDB = $state(false);

	onDestroy(() => form.cleanup());
</script>

<div class="space-y-6">
	<div>
		<h2 class="text-2xl font-bold">Advanced Settings</h2>
		<p class="text-base-content/60 mt-1">
			Control cache freshness, background work, and image loading.
		</p>
	</div>

	{#if form.loading}
		<div class="flex justify-center items-center py-20">
			<span class="loading loading-spinner loading-lg text-primary"></span>
		</div>
	{:else if form.data}
		{#if form.message}
			<div
				class="alert {form.messageType === 'success' ? 'alert-success' : 'alert-error'} alert-soft"
			>
				{#if form.messageType === 'success'}
					<CircleCheck class="w-5 h-5 shrink-0" />
				{:else}
					<CircleAlert class="w-5 h-5 shrink-0" />
				{/if}
				<span>{form.message}</span>
			</div>
		{/if}

		<SettingsSectionCollapse
			title="Page Cache Freshness"
			description="Choose how long pages stay fresh before checking for updates"
			icon={Clock}
			iconBgClass="bg-primary/10"
			iconTextClass="text-primary"
			bind:isOpen={openFrontendCache}
			name="advanced-settings"
		>
			<SettingsFrontendCache bind:data={form.data} />
		</SettingsSectionCollapse>

		<SettingsSectionCollapse
			title="Server Cache Lifetime"
			description="Choose how long the server keeps data before refreshing it from external services"
			icon={Archive}
			iconBgClass="bg-secondary/10"
			iconTextClass="text-secondary"
			bind:isOpen={openBackendCache}
			name="advanced-settings"
		>
			<SettingsBackendCache bind:data={form.data} />
		</SettingsSectionCollapse>

		<SettingsSectionCollapse
			title="Network & Batch Processing"
			description="Set request timeouts, connection limits, and background workload size"
			icon={Globe}
			iconBgClass="bg-accent/10"
			iconTextClass="text-accent"
			bind:isOpen={openNetworkBatch}
			name="advanced-settings"
		>
			<SettingsNetworkBatch bind:data={form.data} />
		</SettingsSectionCollapse>

		<SettingsSectionCollapse
			title="Storage & Discover Queue"
			description="Set cache sizes, disk limits, and Discover Queue background behavior"
			icon={Database}
			iconBgClass="bg-warning/10"
			iconTextClass="text-warning"
			bind:isOpen={openStorageQueue}
			name="advanced-settings"
		>
			<SettingsStorageQueue bind:data={form.data} />
		</SettingsSectionCollapse>

		<SettingsSectionCollapse
			title="AudioDB Images"
			description="Manage artwork from TheAudioDB, including fanart, thumbnails, and logos"
			icon={ImageIcon}
			iconBgClass="bg-primary/10"
			iconTextClass="text-primary"
			bind:isOpen={openAudioDB}
			name="advanced-settings"
		>
			<SettingsAudioDB bind:data={form.data} />
		</SettingsSectionCollapse>

		<div class="flex justify-end gap-3 pt-2">
			<button class="btn btn-ghost" onclick={load} disabled={form.saving}>
				<RotateCcw class="w-4 h-4" />
				Reset
			</button>
			<button class="btn btn-primary" onclick={save} disabled={form.saving}>
				{#if form.saving}
					<span class="loading loading-spinner loading-sm"></span>
					Saving…
				{:else}
					<Save class="w-4 h-4" />
					Save Settings
				{/if}
			</button>
		</div>
	{/if}
</div>
