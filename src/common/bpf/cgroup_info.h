#ifndef CGROUP_INFO_H
#define CGROUP_INFO_H

#define CGROUP_NAME_LEN 256

struct cgroup_info {
	int id;
	u8 name[CGROUP_NAME_LEN];
};

#endif //CGROUP_INFO_H