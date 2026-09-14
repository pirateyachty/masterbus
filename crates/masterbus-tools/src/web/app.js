async function loadDevices() {
    const status = document.getElementById('status');
    const root = document.getElementById('devices');

    status.textContent = 'Scanning devices…';
    root.innerHTML = '';

    const response = await fetch('/api/devices');

    if (!response.ok) {
        document.getElementById('status').textContent =
            'Could not load devices: ' + await response.text();
        return;
    }

    const devices = await response.json();

    document.getElementById('status').textContent =
        devices.length + ' device' + (devices.length === 1 ? '' : 's') + ' discovered';

    for (const device of devices) {
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

        for (const group of device.groups) {
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

document.getElementById('rescan').addEventListener('click', loadDevices);

loadDevices();