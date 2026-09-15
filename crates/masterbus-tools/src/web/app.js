let currentTab = 'all';
let cachedDevices = [];
let signalKElectrical = {};
let currentMapping = {
    version: 1,
    devices: {},
};

function groupVisible(group, mode) {
    if (mode === 'all') return true;
    if (mode === 'config') return group.menu !== 'monitoring';
    return false;
}

async function loadDevices() {
    const status = document.getElementById('status');
    const root = document.getElementById('devices');

    status.textContent = 'Scanning devices…';
    root.innerHTML = '';

    const [devicesResponse, signalKResponse, mappingResponse] =
        await Promise.all([
            fetch('/api/devices'),
            fetch('/api/signalk/electrical'),
            fetch('/api/mapping'),
        ]);

    if (!devicesResponse.ok) {
        status.textContent =
            'Could not load devices: ' + await devicesResponse.text();
        return;
    }

    if (!signalKResponse.ok) {
        status.textContent =
            'Could not load Signal K schema: ' + await signalKResponse.text();
        return;
    }

    if (!mappingResponse.ok) {
        status.textContent =
            'Could not load mapping: ' + await mappingResponse.text();
        return;
    }   

    cachedDevices = await devicesResponse.json();
    signalKElectrical = await signalKResponse.json();
    currentMapping = await mappingResponse.json();

    status.textContent =
        cachedDevices.length + ' device' +
        (cachedDevices.length === 1 ? '' : 's') +
        ' discovered';

    renderDevices();
}

function renderDevices() {
    if (currentTab === 'mapping') {
        renderMapping();
        return;
    }

    renderBrowser();
}

function renderBrowser() {
    const root = document.getElementById('devices');
    root.innerHTML = '';

    for (const device of cachedDevices) {
        const visibleGroups = device.groups.filter(group =>
            groupVisible(group, currentTab)
        );

        if (visibleGroups.length === 0) {
            continue;
        }

        const d = document.createElement('details');
        d.className = 'device';

        const summary = document.createElement('summary');
        summary.textContent = device.name + ' — ' + device.article;
        d.appendChild(summary);

        const meta = document.createElement('div');
        meta.className = 'meta';
        meta.textContent =
            'Serial: ' + device.serial +
            ' | Firmware: ' + device.firmware +
            ' | Status: ' + device.status +
            ' | Address: ' + device.id;
        d.appendChild(meta);

        for (const group of visibleGroups) {
            const g = document.createElement('details');
            g.className = 'group';

            const gs = document.createElement('summary');
            gs.textContent = group.name + ' (' + group.menu + ')';
            g.appendChild(gs);

            const table = document.createElement('table');

            table.innerHTML = `
                <thead>
                    <tr>
                        <th>ID</th>
                        <th>Field</th>
                        <th>Value</th>
                        <th>Unit</th>
                        <th>Mapping</th>
                    </tr>
                </thead>
                <tbody></tbody>
            `;

            const tbody = table.querySelector('tbody');

            for (const field of group.fields) {
                const tr = document.createElement('tr');

                tr.dataset.fieldId = field.id;

                tr.innerHTML = `
                    <td><code>${field.id}</code></td>
                    <td>${field.name}</td>
                    <td>${field.value_text}</td>
                    <td>${field.unit.trim()}</td>
                    <td class="unmapped">unmapped</td>
                `;

                tbody.appendChild(tr);
            }

            g.appendChild(table);
            d.appendChild(g);
        }

        root.appendChild(d);
    }
}

function rankSignalKCandidate(masterBusName, candidatePath) {
    const normalize = value =>
        value
            .toLowerCase()
            .replace(/[^a-z0-9]/g, '');

    const source = normalize(masterBusName);
    const leaf = candidatePath.split('.').pop();
    const target = normalize(leaf);

    let score = 0;

    // Exact semantic word contained in the MasterBus field name.
    if (source.includes(target)) {
        score += 100;
    }

    // Prefer the simpler Signal K leaf when two candidates share a word,
    // e.g. voltage over setpointVoltage for "Battery voltage".
    if (target && source.endsWith(target)) {
        score += 50;
    }

    // Small penalty for additional semantics not present in the source.
    score -= Math.max(0, target.length - source.length);

    return score;
}

async function validateSignalKPath(path, unit) {
    const params = new URLSearchParams({
        path: path,
        unit: unit || '',
    });

    const response = await fetch(
        '/api/signalk/compatibility?' + params.toString()
    );

    if (!response.ok) {
        return {
            valid: false,
            reason: await response.text(),
        };
    }

    const result = await response.json();

    if (!result.compatible) {
        return {
            valid: false,
            reason: result.refusal || 'Incompatible Signal K mapping',
        };
    }

    return {
        valid: true,
        result: result,
    };
}

function renderMapping() {
    const root = document.getElementById('devices');
    root.innerHTML = '';

    for (const device of cachedDevices) {
        const monitoringGroups = device.groups.filter(
            group => group.menu === 'monitoring'
        );

        if (monitoringGroups.length === 0) {
            continue;
        }

        const savedDevice = currentMapping.devices[device.serial] || null;

        const d = document.createElement('details');
        d.className = 'device';

        const totalFields = monitoringGroups.reduce(
            (count, group) => count + group.fields.length,
            0
        );

        const publishedFields = monitoringGroups.reduce(
            (count, group) => {
                return count + group.fields.filter(
                    field => savedDevice?.fields?.[field.id]
                ).length;
            },
            0
        );

        const summary = document.createElement('summary');
        summary.textContent =
            device.name +
            ' — ' +
            device.article +
            ' — ' +
            publishedFields +
            '/' +
            totalFields +
            ' data fields published';

        d.appendChild(summary);

        const meta = document.createElement('div');
        meta.className = 'meta';
        meta.textContent =
            'Serial: ' + device.serial +
            ' | Firmware: ' + device.firmware +
            ' | Status: ' + device.status +
            ' | Address: ' + device.id;
        d.appendChild(meta);

        const identity = document.createElement('div');
        identity.className = 'mapping-identity';

        identity.innerHTML = `
            <div>
                <label>
                    Signal K Type
                    <select class="mapping-type">
                        <option value="">Select type…</option>
                    </select>
                </label>
            </div>

            <div>
                <label>
                    Instance
                    <input
                        class="mapping-instance"
                        type="text"
                        value="${device.name}"
                    >
                </label>
            </div>

            <div class="mapping-base">
                Base path:
                <code class="base-path">electrical.…</code>
            </div>
        `;

        d.appendChild(identity);

        const actions = document.createElement('div');
        actions.className = 'mapping-actions';

        const saveMapping = document.createElement('button');
        saveMapping.type = 'button';
        saveMapping.className = 'mapping-save';
        saveMapping.textContent = 'Save Mapping';
        saveMapping.disabled = true;

        const saveStatus = document.createElement('span');
        saveStatus.className = 'mapping-save-status';

        actions.appendChild(saveMapping);
        actions.appendChild(saveStatus);
        d.appendChild(actions);        

        function markDirty() {
            saveMapping.disabled = false;
            saveStatus.textContent = 'Unsaved changes';
        }

        const typeInput = identity.querySelector('.mapping-type');
        const instanceInput = identity.querySelector('.mapping-instance');
        const basePath = identity.querySelector('.base-path');

        for (const type of Object.keys(signalKElectrical)) {
            const option = document.createElement('option');
            option.value = type;
            option.textContent = type;
            typeInput.appendChild(option);
        }

        if (savedDevice) {
            instanceInput.value = savedDevice.instance || device.name;

            const firstPath = Object.values(savedDevice.fields || {})
                .map(field => field.path)
                .find(path => path?.startsWith('electrical.'));

            if (firstPath) {
                const parts = firstPath.split('.');

                if (parts.length >= 4) {
                    typeInput.value = parts[1];
                }
            }
        }

        updateBasePath();

        function updateBasePath() {
            const type = typeInput.value.trim();
            const instance = instanceInput.value.trim();

            if (type && instance) {
                basePath.textContent =
                    'electrical.' + type + '.' + instance;
            } else {
                basePath.textContent = 'electrical.…';
            }
        }

        typeInput.addEventListener('change', () => {
            updateBasePath();
            markDirty();
        });
        instanceInput.addEventListener('input', () => {
            updateBasePath();
            markDirty();
        });

        for (const group of monitoringGroups) {
            const g = document.createElement('details');
            g.className = 'group';

            const gs = document.createElement('summary');
            gs.textContent = group.name;
            g.appendChild(gs);

            const table = document.createElement('table');

            table.innerHTML = `
                <thead>
                    <tr>
                        <th>Enable</th>
                        <th>Field</th>
                        <th>Value</th>
                        <th>Unit</th>
                        <th>Signal K Field</th>
                        <th>Path</th>
                    </tr>
                </thead>
                <tbody></tbody>
            `;

            const tbody = table.querySelector('tbody');

            for (const field of group.fields) {
                const tr = document.createElement('tr');
                tr.dataset.fieldId = field.id;
                tr.dataset.unit = field.unit || '';

                const enableCell = document.createElement('td');
                const enable = document.createElement('input');
                enable.type = 'checkbox';
                enable.className = 'mapping-enable';
                enableCell.appendChild(enable);

                const fieldCell = document.createElement('td');
                fieldCell.textContent = field.name;

                const valueCell = document.createElement('td');
                valueCell.textContent = field.value_text;

                const unitCell = document.createElement('td');
                unitCell.textContent = field.unit.trim();

                const semanticCell = document.createElement('td');
                
                const semantic = document.createElement('select');
                semantic.className = 'mapping-semantic';
                semantic.disabled = true;
                semanticCell.appendChild(semantic);

                const custom = document.createElement('input');
                custom.type = 'text';
                custom.className = 'mapping-custom';
                custom.placeholder = 'Custom Signal K field';
                custom.hidden = true;
                custom.disabled = true;

                semanticCell.appendChild(custom);

                let customMode = false;                

                const savedField = savedDevice?.fields?.[field.id] || null;

                async function populateSemanticFields() {
                    let selected = semantic.value;

                    if (!selected && savedField?.path) {
                        const prefix =
                            'electrical.' +
                            typeInput.value +
                            '.' +
                            instanceInput.value +
                            '.';

                        if (savedField.path.startsWith(prefix)) {
                            selected = savedField.path.slice(prefix.length);
                        }
                    }
                    const type = typeInput.value;

                    semantic.innerHTML = '';

                    const empty = document.createElement('option');
                    empty.value = '';
                    empty.textContent = 'Select field…';
                    semantic.appendChild(empty);

                    if (!type) {
                        return;
                    }

                    const params = new URLSearchParams({
                        device_type: type,
                        unit: field.unit.trim(),
                    });

                    const response = await fetch(
                        '/api/signalk/candidates?' + params.toString()
                    );

                    if (!response.ok) {
                        return;
                    }

                    const fields = await response.json();

                    fields.sort((a, b) => {
                        return (
                            rankSignalKCandidate(field.name, b.path) -
                            rankSignalKCandidate(field.name, a.path)
                        );
                    });                    

                    for (const signalKField of fields) {
                        const option = document.createElement('option');
                        option.value = signalKField.path;

                        option.textContent = signalKField.unit
                            ? signalKField.path + ' (' + signalKField.unit + ')'
                            : signalKField.path;

                        semantic.appendChild(option);
                    }

                    if (fields.some(candidate => candidate.path === selected)) {
                        semantic.value = selected;
                    } else if (fields.length > 0) {
                        semantic.value = fields[0].path;
                    }
                }

                const pathCell = document.createElement('td');
                const path = document.createElement('code');
                path.className = 'mapping-path';
                path.textContent = '—';

                const edit = document.createElement('button');
                edit.type = 'button';
                edit.textContent = 'Edit';
                edit.disabled = true;
                edit.className = 'mapping-edit';

                pathCell.appendChild(path);
                pathCell.appendChild(document.createTextNode(' '));
                pathCell.appendChild(edit);

                function updatePath() {
                    const type = typeInput.value.trim();
                    const instance = instanceInput.value.trim();
                    const leaf = customMode
                        ? custom.value.trim()
                        : semantic.value.trim();

                    if (enable.checked && type && instance && leaf) {
                        path.textContent =
                            'electrical.' +
                            type + '.' +
                            instance + '.' +
                            leaf;
                    } else {
                        path.textContent = '—';
                    }
                }

                enable.addEventListener('change', () => {
                    semantic.disabled = !enable.checked;
                    edit.disabled = !enable.checked;
                    updatePath();
                    markDirty();
                });

                semantic.addEventListener('change', () => {
                    updatePath();
                    markDirty();
                });

                edit.addEventListener('click', () => {
                    customMode = !customMode;

                    if (customMode) {
                        custom.value = semantic.value;
                        semantic.hidden = true;
                        semantic.disabled = true;

                        custom.hidden = false;
                        custom.disabled = false;
                        custom.focus();

                        edit.textContent = 'Reset';
                    } else {
                        custom.value = '';
                        custom.hidden = true;
                        custom.disabled = true;

                        semantic.hidden = false;
                        semantic.disabled = !enable.checked;

                        edit.textContent = 'Edit';
                    }

                    updatePath();
                    markDirty();
                });

                custom.addEventListener('input', () => {
                    updatePath();
                    markDirty();
                });                

                typeInput.addEventListener('change', () => {
                    populateSemanticFields();
                    updatePath();
                });
                instanceInput.addEventListener('input', updatePath);

                if (savedField) {
                    enable.checked = true;
                    edit.disabled = false;

                    populateSemanticFields().then(() => {
                        const prefix =
                            'electrical.' +
                            typeInput.value +
                            '.' +
                            instanceInput.value +
                            '.';

                        const savedLeaf = savedField.path.startsWith(prefix)
                            ? savedField.path.slice(prefix.length)
                            : '';

                        const isSchemaField = Array.from(semantic.options).some(
                            option => option.value === savedLeaf
                        );

                        if (isSchemaField) {
                            customMode = false;

                            semantic.value = savedLeaf;
                            semantic.hidden = false;
                            semantic.disabled = false;

                            custom.hidden = true;
                            custom.disabled = true;

                            edit.textContent = 'Edit';
                        } else {
                            customMode = true;

                            custom.value = savedLeaf;
                            custom.hidden = false;
                            custom.disabled = false;

                            semantic.hidden = true;
                            semantic.disabled = true;

                            edit.textContent = 'Reset';
                        }

                        updatePath();
                    });
                } else {
                    populateSemanticFields();
                }

                tr.appendChild(enableCell);
                tr.appendChild(fieldCell);
                tr.appendChild(valueCell);
                tr.appendChild(unitCell);
                tr.appendChild(semanticCell);
                tr.appendChild(pathCell);

                tbody.appendChild(tr);
            }

            g.appendChild(table);
            d.appendChild(g);
        }

        saveMapping.addEventListener('click', async () => {
            d.querySelectorAll('.mapping-validation-error').forEach(row => {
                row.classList.remove('mapping-validation-error');
            });
            const type = typeInput.value.trim();
            const instance = instanceInput.value.trim();

            const fields = {};

            for (const row of d.querySelectorAll('tr[data-field-id]')) {
                const enable = row.querySelector('.mapping-enable');
                const semantic = row.querySelector('.mapping-semantic');
                const custom = row.querySelector('.mapping-custom');

                if (!enable?.checked) {
                    continue;
                }

                const leaf = !custom.hidden
                    ? custom.value.trim()
                    : semantic.value.trim();

                if (!leaf) {
                    saveStatus.textContent =
                        'Validation failed: enabled field has no Signal K field';
                    return;
                }

                const path =
                    'electrical.' +
                    type + '.' +
                    instance + '.' +
                    leaf;

                const unit = row.dataset.unit || '';

                const validation = await validateSignalKPath(path, unit);

                if (!validation.valid) {
                    row.classList.add('mapping-validation-error');

                    const fieldName =
                        row.querySelector('td:nth-child(2)')?.textContent?.trim() ||
                        row.dataset.fieldId;

                    saveStatus.textContent =
                        fieldName + ': ' + validation.reason;

                    console.error(
                        'Signal K validation failed:',
                        path,
                        unit,
                        validation.reason
                    );

                    return;
                }

                fields[row.dataset.fieldId] = {
                    path: path,
                };
            }

            if (Object.keys(fields).length === 0) {
                delete currentMapping.devices[device.serial];
            } else {
                currentMapping.devices[device.serial] = {
                    article: device.article,
                    firmware: device.firmware,
                    name: device.name,
                    instance: instance,
                    fields: fields,
                };
            }

            saveMapping.disabled = true;
            saveStatus.textContent = 'Saving…';

            const response = await fetch('/api/mapping', {
                method: 'PUT',
                headers: {
                    'Content-Type': 'application/json',
                },
                body: JSON.stringify(currentMapping),
            });

            if (!response.ok) {
                saveMapping.disabled = false;
                saveStatus.textContent = 'Save failed';
                console.error(await response.text());
                return;
            }

            currentMapping = await response.json();
            saveStatus.textContent = 'Saved';
        });

        root.appendChild(d);
    }
}

document.querySelectorAll('.tab').forEach(button => {
    button.addEventListener('click', () => {
        currentTab = button.dataset.tab;

        document.querySelectorAll('.tab').forEach(tab => {
            tab.classList.toggle('active', tab === button);
        });

        renderDevices();
    });
});

document.getElementById('rescan').addEventListener('click', loadDevices);

loadDevices();
