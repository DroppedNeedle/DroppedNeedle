<script lang="ts">
	import { SETTINGS_ENDPOINTS } from '$lib/queries/settings/endpoints';
	import { CircleCheck, CircleAlert, Database, RotateCcw, Save, ImageIcon } from 'lucide-svelte';
	import { createSettingsForm } from '$lib/utils/settingsForm.svelte';
	import { onDestroy } from 'svelte';
	import type { AdvancedSettingsForm } from './advanced-settings-types';
	import SettingsSectionCollapse from './SettingsSectionCollapse.svelte';
	import SettingsStorageQueue from './SettingsStorageQueue.svelte';
	import SettingsAudioDB from './SettingsAudioDB.svelte';
	import AsyncButton from '$lib/ui/AsyncButton.svelte';
	import Button from '$lib/ui/Button.svelte';

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

	let openStorageQueue = $state(true);
	let openAudioDB = $state(false);

	onDestroy(() => form.cleanup());
</script>

<div class="space-y-6">
	<div>
		<h2 class="text-2xl font-bold">Advanced Settings</h2>
		<p class="text-base-content/60 mt-1">Control the Discover Queue and image loading.</p>
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
			title="Discover Queue"
			description="Set how the Discover Queue and Discover Picks are built"
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
			<Button variant="ghost" icon={RotateCcw} onclick={load} disabled={form.saving}>Reset</Button>
			<AsyncButton
				variant="primary"
				icon={Save}
				onclick={save}
				pending={form.saving}
				pendingLabel="Saving…"
			>
				Save Settings
			</AsyncButton>
		</div>
	{/if}
</div>
